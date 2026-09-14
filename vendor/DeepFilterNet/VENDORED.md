# Vendored DeepFilterNet (libDF)

- **Source:** https://github.com/Rikorose/DeepFilterNet (official repository)
- **Commit:** `d375b2d8309e0935d165700c91da9de862a99c31` (main, 2024-09-25)
- **Copied:** `libDF/`, `models/DeepFilterNet3_onnx.tar.gz`, `models/DeepFilterNet3_ll_onnx.tar.gz`, `LICENSE*`, `README.md`
- **License:** MIT OR Apache-2.0 (license files kept alongside)
- **Model SHA-256 (identical to upstream):**
  - `DeepFilterNet3_onnx.tar.gz` `C94D91F70911001C946E0FABB4AA9ADC37045F45A03B56008CB0C8244CB63616`
  - `DeepFilterNet3_ll_onnx.tar.gz` `5998E58E8BA0E09BB76986EF97B84AFA065A571EF282D4A1222F341E3251CF3A`

AudioManager embeds the low-latency model itself (`src-tauri/src/dsp/denoise.rs`) for the
"Low-latency model" switch, rather than through libDF's `default-model-ll` feature.

## Local changes

Upstream only compiles against tract 0.21.4, whose graph optimizer fails to load the DFN3 model
("duplicate name /convt3/Conv.bias"). These edits port it to current tract 0.21.x and ndarray 0.16:

`libDF/Cargo.toml`
```diff
-ndarray = { version = "^0.15", optional = true, features = ["serde"] }
-ndarray-rand = { version = "^0.14", optional = true }
+ndarray = { version = "^0.16", optional = true, features = ["serde"] }
+ndarray-rand = { version = "^0.15", optional = true }
```

`libDF/src/tract.rs` (3 places: init_encoder_impl, init_erb_decoder_impl, init_df_decoder_impl)
```diff
-    let s = m.symbol_table.sym("S");
+    let s = m.symbols.sym("S");
```

Nothing else differs from upstream.
