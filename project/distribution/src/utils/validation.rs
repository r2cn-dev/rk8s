use oci_spec::image::Digest;
use regex::Regex;
use std::str::FromStr;

pub fn is_valid_name(name: &str) -> bool {
    let re =
        Regex::new(r"^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$")
            .unwrap();
    re.is_match(name)
}

pub fn is_valid_digest(digest: &str) -> bool {
    Digest::from_str(digest).is_ok()
}

pub fn is_valid_tag(tag: &str) -> bool {
    let re = Regex::new(r"^[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}$").unwrap();
    re.is_match(tag)
}

pub fn is_valid_reference(reference: &str) -> bool {
    is_valid_digest(reference) || is_valid_tag(reference)
}

pub fn is_valid_range(range: &str) -> bool {
    let re = Regex::new(r"^[0-9]+-[0-9]+$").unwrap();
    re.is_match(range)
}
