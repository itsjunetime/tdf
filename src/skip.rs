use std::num::NonZeroUsize;

use ratatui::widgets::Widget;

pub struct Skip {
	skip: bool
}

impl Skip {
	#[must_use]
	pub fn new(skip: bool) -> Self {
		Self { skip }
	}
}

impl Widget for Skip {
	fn render(self, area: ratatui::prelude::Rect, buf: &mut ratatui::prelude::Buffer) {
		for x in area.x..(area.x + area.width) {
			for y in area.y..(area.y + area.height) {
				buf[(x, y)].skip = self.skip;
			}
		}
	}
}

enum PlusOrMinus {
	Plus,
	Minus
}

pub struct InterleavedAroundWithMax {
	// starts at this number
	around: usize,
	inclusive_min: usize,
	// this iterator can only produce values in [0..max)
	exclusive_max: NonZeroUsize,
	// the next time we call `next()`, this value should be combined with `around` according to
	// `next_op`, then, after next_op is inverted, incremented if next_op was negative before being
	// inverted.
	next_change: usize,
	// How `next_change` should be applied to `around` next time `next()` is called
	next_op: PlusOrMinus
}

impl InterleavedAroundWithMax {
	/// the following must hold or else this is liable to panic or produce nonsense values:
	/// - inclusive_min < exclusive_max
	/// - inclusive_min <= around <= exclusive_max
	#[must_use]
	pub fn new(around: usize, inclusive_min: usize, exclusive_max: NonZeroUsize) -> Self {
		Self {
			around,
			inclusive_min,
			exclusive_max,
			next_change: 0,
			next_op: PlusOrMinus::Minus
		}
	}
}

impl Iterator for InterleavedAroundWithMax {
	type Item = usize;
	fn next(&mut self) -> Option<Self::Item> {
		let actual_change = self.next_change % (self.exclusive_max.get() - self.inclusive_min);

		let to_return = match self.next_op {
			// If we're supposed to add them and we need it to wrap, then try to add them together
			// 'cause we need special behavior if it overflows usize's limits
			PlusOrMinus::Plus => match self.around.checked_add(actual_change) {
				// If we added it and it's within the range, we're chillin
				Some(next_val) if next_val < self.exclusive_max.get() => next_val,
				// If we added it and it's not within the range, do next_val % (self.max + 1), e.g.
				// if max is 20, we were at 15, and we added 7, we should get 1 (because +5 would
				// hit the max, then 0, then 1). So adding 1 before the modulo makes it hit the
				// right numbers. And we can be sure the + here doesn't overflow 'cause we already
				// checked the `usize::MAX` up above
				Some(next_val) => (next_val % self.exclusive_max.get()) + self.inclusive_min,
				// If we added them and it would've overflowed usize::MAX, then we see how much
				// of the change would be remaining after reaching `max`
				None =>
					(actual_change - (self.exclusive_max.get() - actual_change))
						+ self.inclusive_min,
			},
			PlusOrMinus::Minus => match self.around.checked_sub(actual_change) {
				// If we can just minus it, cool cool. All is good.
				Some(next_val) if next_val >= self.inclusive_min => next_val,
				// If we can minus it but it goes below our min, then see how much below it went
				// and just manually wrap it around
				Some(next_val) => self.exclusive_max.get() - (self.inclusive_min - next_val),
				// If we can't...
				None => {
					// then we see how much of the change would be remaining after hitting the
					// minimum
					let remaining = actual_change - (self.around - self.inclusive_min);

					// and then we take that away from the top!
					self.exclusive_max.get() - remaining
				}
			}
		};

		self.next_op = match self.next_op {
			PlusOrMinus::Plus => PlusOrMinus::Minus,
			PlusOrMinus::Minus => {
				self.next_change = (self.next_change + 1) % self.exclusive_max.get();
				PlusOrMinus::Plus
			}
		};

		Some(to_return)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn iter_works() {
		let got = InterleavedAroundWithMax::new(5, 2, NonZeroUsize::new(21).unwrap())
			.take(30)
			.collect::<Vec<_>>();

		assert_eq!(got, vec![
			5, 6, 4, 7, 3, 8, 2, 9, 20, 10, 19, 11, 18, 12, 17, 13, 16, 14, 15, 15, 14, 16, 13, 17,
			12, 18, 11, 19, 10, 20
		]);
	}
}
