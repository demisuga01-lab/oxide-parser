pub(crate) mod cid;
pub mod cmap;
pub mod encoding;
pub mod glyph_list;
pub mod resolver;
pub(crate) mod type1;
pub mod variations;

pub use resolver::{FontResolver, FontType};
pub use variations::{AxisValue, VariationRequest};
