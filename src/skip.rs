use ratatui::widgets::Widget;

pub struct Skip {
	skip: bool
}

impl Skip {
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
