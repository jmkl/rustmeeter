use cactus_core::{Cactus, Models, Transcriber, Whistle};
use std::path::PathBuf;

#[test]
fn probe_whistle() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = root.join("cactus").join("whistle.cact");
    println!("model: {}", model.display());

    let mut engine = Cactus::new();
    let m = engine.load(&model).unwrap();
    println!("models: {:?}", m);
    assert!(m.contains(Models::SPEECH));
    assert!(!m.contains(Models::TEXT));

    let mut w = Whistle::load(&model, Some("en")).unwrap();
    w.set_threads(2);

    // silence clip (1 s of zeros)
    let pcm16k: Vec<i16> = vec![0i16; 16_000];
    let txt = w.transcribe(&pcm16k).unwrap();
    println!("silence -> {:?}", txt);

    // stream path
    let chunk: Vec<i16> = vec![0i16; 16_000];
    let j1 = w.stream_process(&chunk, None).unwrap();
    println!("stream_process: {}", j1);
    let j2 = w.stream_stop().unwrap();
    println!("stream_stop: {}", j2);

    // set_audio
    engine.set_audio(Some("en"), Some("Siobhan"), true);

    // describe on a speech model should bail
    let rgb = vec![0u8; 4 * 4 * 3];
    let r = engine.describe(&rgb, 4, 4, None, None, None);
    assert!(r.is_err(), "describe should fail on speech-only engine");

    // init on a speech model should bail
    let r = engine.init(None, None, None);
    assert!(r.is_err(), "init should fail on speech-only engine");

    // cactus_complete on a speech model: text capability missing -> bail
    let r = engine.complete(Some("hello"), None, 16);
    assert!(r.is_err(), "complete should fail on speech-only engine");

    println!("OK");
}
