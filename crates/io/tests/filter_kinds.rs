//! The filter kinds added after the format was fixed serialise as tagged
//! JSON next to the old ones, and old filter JSON still reads.

use lumenply_doc::Filter;

#[test]
fn new_filter_kinds_round_trip_and_old_ones_still_read() {
    let kinds = [
        Filter::Mosaic { size: 12.0 },
        Filter::Emboss {
            angle: 135.0,
            height: 3.0,
            amount: 1.5,
        },
        Filter::FindEdges,
        Filter::SurfaceBlur {
            radius: 5.0,
            threshold: 15.0,
        },
        Filter::LensBlur {
            radius: 10.0,
            highlights: 0.3,
        },
        Filter::DustScratches {
            radius: 2.0,
            threshold: 10.0,
        },
    ];
    for f in kinds {
        let s = serde_json::to_string(&f).unwrap();
        let back: Filter = serde_json::from_str(&s).unwrap();
        assert_eq!(back, f, "{s}");
    }
    assert_eq!(
        serde_json::to_string(&Filter::FindEdges).unwrap(),
        r#"{"type":"find-edges"}"#
    );
    assert_eq!(
        serde_json::to_string(&Filter::DustScratches {
            radius: 2.0,
            threshold: 10.0
        })
        .unwrap(),
        r#"{"type":"dust-scratches","radius":2.0,"threshold":10.0}"#
    );
    // A filter layer from before these kinds existed.
    let old: Filter = serde_json::from_str(r#"{"type":"gaussian-blur","radius":3.5}"#).unwrap();
    assert_eq!(old, Filter::GaussianBlur { radius: 3.5 });
}
