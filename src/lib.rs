//! copperpath: rule-aware placement, routability check and a grid router for KiCad 10 boards.
//!
//! The library reads `.kicad_pcb`, `.kicad_pro` (net classes) and `.kicad_dru` (custom rules)
//! and builds a net-level rule model. The binary (`src/main.rs`) exposes the commands
//! `check`, `place`, `route`, `opens`, `hints`, `pads` and `netclass`.

pub mod board;
pub mod check;
pub mod clean;
pub mod droute;
pub mod geom;
pub mod groute;
pub mod img;
pub mod model;
pub mod place;
pub mod rules;
pub mod sch;
pub mod sexpr;
pub mod sites;
