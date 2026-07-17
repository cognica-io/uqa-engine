//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("emscripten") {
        return;
    }
    let link_args = [
        // ES-module factory (`createUQAModule`) instead of a global.
        "-sMODULARIZE=1",
        "-sEXPORT_NAME=createUQAModule",
        "-sEXPORT_ES6=1",
        "-sALLOW_MEMORY_GROWTH=1",
        "-sENVIRONMENT=web,worker,node",
        // The PostgreSQL parser recurses deeply; the emscripten default
        // 64 KiB stack overflows on real queries.
        "-sSTACK_SIZE=5242880",
        // SQLite databases live on the emscripten virtual filesystem;
        // IDBFS mounts persist them into IndexedDB in the browser.
        "-sFORCE_FILESYSTEM=1",
        "-lidbfs.js",
        "-sEXPORTED_FUNCTIONS=_uqa_call,_uqa_free,_main,_malloc,_free",
        "-sEXPORTED_RUNTIME_METHODS=ccall,cwrap,UTF8ToString,lengthBytesUTF8,stringToUTF8,FS,IDBFS",
    ];
    for arg in link_args {
        println!("cargo:rustc-link-arg={arg}");
    }
}
