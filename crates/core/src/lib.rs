//! MacCleaner core: junk rules, scanning, cleaning, space view,
//! system metrics and monitor registration.

pub mod access;
pub mod apps;
pub mod clean;
pub mod clutter;
pub mod codesign;
pub mod dev;
pub mod fsutil;
pub mod governor;
pub mod launchd;
pub mod launchservices;
pub mod performance;
pub mod privacy;
pub mod progress;
pub mod rules;
pub mod scan;
pub mod space;
pub mod state;
pub mod system;
pub mod trash;

#[cfg(test)]
mod testutil;
