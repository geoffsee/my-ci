use std::path::Path;

use anyhow::Result;
use bollard::Docker;

const PROVIDERS: [OciProvider; 2] = [OciProvider::Docker, OciProvider::Podman];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OciProvider {
    Docker,
    Podman,
}

pub fn detect_oci_provider() -> Option<OciProvider> {
    for &provider in &PROVIDERS {
        if Path::new(get_oci_socket_addr(provider)).exists() {
            return Some(provider);
        }
    }
    None
}

pub fn connect_oci(provider: OciProvider) -> Result<Docker> {
    match provider {
        OciProvider::Docker => Ok(Docker::connect_with_unix_defaults()?),
        // todo: use podman client
        OciProvider::Podman => Ok(Docker::connect_with_unix_defaults()?),
    }
}

pub fn get_oci_socket_addr(oci_provider: OciProvider) -> &'static str {
    match oci_provider {
        OciProvider::Docker => "/var/run/docker.sock",
        OciProvider::Podman => "/var/run/podman/podman.sock",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_addr_for_docker() {
        assert_eq!(get_oci_socket_addr(OciProvider::Docker), "/var/run/docker.sock");
    }

    #[test]
    fn socket_addr_for_podman() {
        assert_eq!(
            get_oci_socket_addr(OciProvider::Podman),
            "/var/run/podman/podman.sock"
        );
    }

    #[test]
    fn detect_returns_none_when_no_sockets() {
        let docker_exists = Path::new("/var/run/docker.sock").exists();
        let podman_exists = Path::new("/var/run/podman/podman.sock").exists();
        let detected = detect_oci_provider();
        if !docker_exists && !podman_exists {
            assert!(detected.is_none());
        } else {
            assert!(detected.is_some());
        }
    }
}
