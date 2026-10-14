pub fn profile_label(name: &str, active: bool) -> String {
    if active {
        format!("● {name}")
    } else {
        name.to_string()
    }
}
