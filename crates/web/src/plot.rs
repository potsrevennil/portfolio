//! Charts drawn on the server: SVG lines over an HTML frame, HTML columns,
//! and SVG donuts, sized to stretch to any width with no script. Once
//! hydrated, a readout follows the pointer or the arrow keys across the
//! periods.

pub mod axis;
pub mod column;
pub mod frame;
pub mod line;
pub mod pie;

pub use column::ColumnChart;
pub use line::LineChart;
pub use pie::PieChart;
