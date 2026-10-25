// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the scan makes of LV2 descriptions the test plugins don't have
//! themselves (`urn:dawkey:test:gain`'s library described by a TTL file of
//! the test's own).

use std::time::Duration;

use dawkey_engine::{
    MAX_PLUGIN_EVENTS, MidiSink, PluginEvent, PluginProcessor, ProcessContext, Transport,
};
use dawkey_plugins::{Host, Loaded, SearchPath, scan};

#[global_allocator]
static ALLOCATOR: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// One block of `input` on both channels: the left output.
fn run(p: &mut Box<dyn PluginProcessor>, input: &[f32]) -> Vec<f32> {
    let mut left = input.to_vec();
    let mut right = input.to_vec();
    let mut sent: Vec<PluginEvent> = Vec::with_capacity(MAX_PLUGIN_EVENTS);
    assert_no_alloc::assert_no_alloc(|| {
        p.process(
            [&mut left, &mut right],
            ProcessContext::new(Transport::default()),
            &[],
            MidiSink::new(&mut sent),
        );
    });
    left
}

fn unload(loaded: Loaded) {
    loaded.instance.unload(loaded.processor.into_any());
}

/// An LV2 bundle in a folder of its own that describes `urn:dawkey:test:gain`
/// (from the test library) with `gain_port` as its control port.
fn custom_gain(gain_port: &str) -> (tempfile::TempDir, SearchPath) {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("custom.lv2");
    std::fs::create_dir(&bundle).unwrap();
    std::os::unix::fs::symlink(
        dawkey_test_plugins::library().unwrap(),
        bundle.join("libdawkey_test_plugins.so"),
    )
    .unwrap();
    let ttl = format!(
        r#"@prefix doap: <http://usefulinc.com/ns/doap#> .
@prefix lv2: <http://lv2plug.in/ns/lv2core#> .
@prefix units: <http://lv2plug.in/ns/extensions/units#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:dawkey:test:gain>
    a lv2:Plugin ;
    lv2:binary <libdawkey_test_plugins.so> ;
    doap:name "Custom Gain" ;
    lv2:port [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 0 ; lv2:symbol "in_l" ; lv2:name "In L"
    ] , [
        a lv2:InputPort , lv2:AudioPort ; lv2:index 1 ; lv2:symbol "in_r" ; lv2:name "In R"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 2 ; lv2:symbol "out_l" ; lv2:name "Out L"
    ] , [
        a lv2:OutputPort , lv2:AudioPort ; lv2:index 3 ; lv2:symbol "out_r" ; lv2:name "Out R"
    ] , [
        a lv2:InputPort , lv2:ControlPort ; lv2:index 4 ; lv2:symbol "gain" ; lv2:name "Gain" ;
        {gain_port}
    ] .
"#
    );
    std::fs::write(bundle.join("manifest.ttl"), ttl).unwrap();
    let search = SearchPath {
        clap: Vec::new(),
        lv2: Some(vec![dir.path().to_owned()]),
        vst3: Vec::new(),
    };
    (dir, search)
}

/// The gain (in dB) the custom gain plays at when loaded at `sample_rate`.
fn plays_at(search: &SearchPath, sample_rate: u32) -> f32 {
    let catalog = scan(search, None, Duration::from_secs(10));
    let info = catalog.get("lv2:urn:dawkey:test:gain").expect("scanned");
    let mut host = Host::new(sample_rate, search.clone());
    let mut loaded = host.load(info).unwrap();
    let out = run(&mut loaded.processor, &[0.5; 64]);
    unload(loaded);
    20.0 * (out[0] / 0.5).log10()
}

/// `lv2:minimum` and `lv2:maximum` are hints a port may leave out, and
/// `lv2:default` is what it starts at: a port without a maximum keeps its
/// minimum and default (it used to get 0 to 1, and start at 1 dB).
#[test]
fn an_lv2_port_without_a_maximum_keeps_its_default() {
    let (_dir, search) = custom_gain("lv2:default 6.0 ; lv2:minimum -60.0 ; units:unit units:db");
    let catalog = scan(&search, None, Duration::from_secs(10));
    let gain = &catalog
        .get("lv2:urn:dawkey:test:gain")
        .expect("scanned")
        .params[0];
    assert_eq!((gain.min, gain.default), (-60.0, 6.0), "{gain:?}");
    assert!(gain.max >= 6.0, "{gain:?}");
    let db = plays_at(&search, 48_000);
    assert!((db - 6.0).abs() < 1e-3, "the plugin plays at {db} dB");
}

/// An enumeration with scale points (0 to 3) and a default (2) but no
/// bounds gets a range that holds them all (it used to be 0 to 1).
#[test]
fn an_lv2_enumeration_without_bounds_keeps_its_choices() {
    let (_dir, search) = custom_gain(
        "lv2:portProperty lv2:enumeration ; lv2:default 2 ; \
         lv2:scalePoint [ rdfs:label \"A\" ; rdf:value 0 ] , [ rdfs:label \"B\" ; rdf:value 1 ] , \
         [ rdfs:label \"C\" ; rdf:value 2 ] , [ rdfs:label \"D\" ; rdf:value 3 ]",
    );
    let catalog = scan(&search, None, Duration::from_secs(10));
    let p = &catalog
        .get("lv2:urn:dawkey:test:gain")
        .expect("scanned")
        .params[0];
    assert_eq!((p.min, p.max, p.default), (0.0, 3.0, 2.0), "{p:?}");
}

/// Declared bounds still hold: a default outside them is moved in, and a
/// port with neither bounds nor a default gets 0 to 1, starting at 0.
#[test]
fn declared_lv2_bounds_hold_the_default() {
    let (_dir, search) = custom_gain(
        "lv2:default 20.0 ; lv2:minimum -60.0 ; lv2:maximum 12.0 ; units:unit units:db",
    );
    let catalog = scan(&search, None, Duration::from_secs(10));
    let p = &catalog
        .get("lv2:urn:dawkey:test:gain")
        .expect("scanned")
        .params[0];
    assert_eq!((p.min, p.max, p.default), (-60.0, 12.0, 12.0), "{p:?}");
    let (_dir, search) = custom_gain("units:unit units:db");
    let catalog = scan(&search, None, Duration::from_secs(10));
    let p = &catalog
        .get("lv2:urn:dawkey:test:gain")
        .expect("scanned")
        .params[0];
    assert_eq!((p.min, p.max, p.default), (0.0, 1.0, 0.0), "{p:?}");
}
