//! Small display and domain helpers shared by the panels.

/// Keeps a hair below zero from rendering as `-0`.
pub(crate) fn zeroed(db: f32) -> f32 {
    if db.abs() < 0.05 { 0.0 } else { db }
}

pub(crate) fn trim_db(db: f32) -> String {
    format!("{:+.0}", zeroed(db))
}

pub(crate) fn bus_name(bus: usize) -> &'static str {
    if bus == 0 { "A" } else { "B" }
}
