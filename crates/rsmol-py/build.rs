fn main() {
    // Python symbols are resolved when the interpreter loads the extension (macOS needs
    // `-undefined dynamic_lookup` for plain cargo builds; maturin adds it on its own).
    pyo3_build_config::add_extension_module_link_args();
}
