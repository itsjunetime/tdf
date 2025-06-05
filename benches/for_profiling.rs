mod utils;

const BLACK: i32 = 0;
const WHITE: i32 = i32::from_be_bytes([0, 0xff, 0xff, 0xff]);

#[tokio::main]
async fn main() {
	#[cfg(feature = "tracing")]
	console_subscriber::init();

	let file = std::env::args()
		.nth(1)
		.expect("Please enter a file to profile");

	utils::render_doc(file, None, BLACK, WHITE).await;
}
