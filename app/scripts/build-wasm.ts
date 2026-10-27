// Builds crates/rsmol-web to WebAssembly and generates its JS bindings in src/lib/wasm/, for
// the webview viewport (Linux, Windows). Always a release build: unoptimized wasm is far too
// slow to parse large structures. Needs `rustup target add wasm32-unknown-unknown` and
// wasm-bindgen-cli matching the wasm-bindgen version in Cargo.lock.
import { $ } from "bun";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../..", import.meta.url));
const target = `${root}/target/wasm`;

await $`cargo build --release -p rsmol-web --target wasm32-unknown-unknown`
  .cwd(root)
  .env({ ...process.env, CARGO_TARGET_DIR: target });
await $`wasm-bindgen --target web --out-dir ${root}/app/src/lib/wasm ${target}/wasm32-unknown-unknown/release/rsmol_web.wasm`;
