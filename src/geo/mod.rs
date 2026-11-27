// ABOUTME: Geometry on (latitude, longitude) points in degrees: the great-circle distance between two
// ABOUTME: The encoded-polyline codec route overviews travel in lives in the polyline submodule
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # Geometry
//!
//! Points are `(latitude, longitude)` pairs in degrees, the shape GPS
//! recordings and encoded polylines carry. [`meters_between`] is the one
//! great-circle distance, and [`polyline`] the one encoded-polyline codec.

pub mod polyline;

const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// The great-circle distance between two `(latitude, longitude)` points.
#[must_use]
pub fn meters_between(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat_a, lat_b) = (a.0.to_radians(), b.0.to_radians());
    let half_lat = (lat_b - lat_a) / 2.0;
    let half_lon = (b.1 - a.1).to_radians() / 2.0;
    let h = lat_a
        .cos()
        .mul_add(lat_b.cos() * half_lon.sin().powi(2), half_lat.sin().powi(2));
    2.0 * EARTH_RADIUS_METERS * h.sqrt().min(1.0).asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_degree_of_latitude_is_about_111_km() {
        let meters = meters_between((45.0, -73.0), (46.0, -73.0));
        assert!((meters - 111_195.0).abs() < 100.0, "bug: {meters}");
    }

    /// A parallel shrinks with the cosine of its latitude: at 60 degrees, half.
    #[test]
    fn a_degree_of_longitude_shrinks_with_latitude() {
        let meters = meters_between((60.0, -73.0), (60.0, -72.0));
        assert!((meters - 55_597.0).abs() < 100.0, "bug: {meters}");
    }
}
