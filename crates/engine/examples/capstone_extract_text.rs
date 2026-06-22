use std::path::PathBuf;

use oxide_engine::{ContentEngine, Result};

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("fixtures")
                .join("basicapi.pdf")
        });
    let engine = ContentEngine::open_path(path)?;
    print!("{}", engine.get_page_text(1)?);
    Ok(())
}
