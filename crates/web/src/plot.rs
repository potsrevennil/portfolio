//! Charts drawn on the server: SVG lines over an HTML frame, HTML columns and
//! bars, all sized in percentages so they stretch to any width with no
//! script. Once hydrated, a readout follows the pointer or the arrow keys
//! across the periods; before that, each period's figures are its tooltip,
//! and every chart has its table.

pub mod axis;
pub mod column;
pub mod figure;
pub mod frame;
pub mod line;
pub mod shares;

pub use axis::{Compact, Scale};
pub use column::ColumnChart;
pub use figure::FigureText;
pub use frame::TableView;
pub use line::LineChart;
pub use shares::ShareBars;
