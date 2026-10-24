//! rsmol-core: molecular data model, file I/O, bonds, spatial queries and selections.
//!
//! This crate has no UI or GPU dependencies so it can be used from the desktop app, WASM and
//! the Python bindings alike.

pub mod backbone;
pub mod bonds;
mod element;
pub mod io;
pub mod select;
pub mod spatial;
mod structure;

pub use element::Element;
pub use structure::{
    Atoms, Bond, BondKind, Chain, Entity, EntityKind, Residue, SecondaryStructure, Structure, Vec3,
};
