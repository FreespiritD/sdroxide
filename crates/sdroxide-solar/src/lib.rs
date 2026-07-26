//! Solar-system ephemeris and space-weather data for the sdroxide 3D view.
//!
//! Native-only: this crate opens outbound HTTPS connections and touches the
//! filesystem, so it must never become a dependency of a wasm-targeted crate.
//!
//! Two halves, deliberately separable:
//!
//! * [`ephem`] — pure arithmetic. No I/O, no threads, fully unit-tested against
//!   the worked examples in Meeus.
//! * the data layer — DONKI coronal mass ejections and flares, NOAA SWPC
//!   sunspot regions and aurora, and SDO solar imagery, fetched on a background
//!   thread and cached to disk so the view opens instantly and survives being
//!   offline.

pub mod aurora;
pub mod cache;
pub mod donki;
pub mod feed;
pub mod ephem;
pub mod helio;
pub mod imagery;
pub mod indices;
pub mod impact;
pub mod planets;
pub mod satellites;
pub mod swpc;
pub mod timefmt;
pub mod vec3;

pub use aurora::{AuroraOval, HemisphericPower, KpPoint};
pub use donki::{CmeAnalysis, CmeEvent, FlareEvent};
pub use ephem::{AU, EARTH_R, MOON_R, SUN_R, SunFrame};
pub use imagery::{SdoChannel, SunImage};
pub use indices::{GeomagneticIndex, MufEstimate, SolarFlux, SpaceWeather, XrayLevel};
pub use impact::{Impact, earth_impact};
pub use planets::{Moon, Planet, Surface};
pub use feed::{FeedCmd, SolarData, SolarFeed, Source, SourceStatus};
pub use satellites::{Pass, PassSearch, SatState, Satellite};
pub use swpc::ActiveRegion;
pub use vec3::{Basis, Vec3, vec3};
