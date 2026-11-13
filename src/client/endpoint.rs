mod activation;
mod catalog;
mod control;
mod health;
mod message_policy;
mod registry;
mod ssh_metadata;
mod supervisor;
mod writer;

pub(crate) use activation::*;
pub(crate) use catalog::*;
pub(crate) use control::*;
pub(crate) use message_policy::*;
pub(crate) use registry::*;
pub(crate) use ssh_metadata::{SshMachineMetadata, SshMetadataCache};
pub(crate) use supervisor::*;
pub(crate) use writer::NativeEndpointTransport;

pub(crate) use super::endpoint_model::{ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_ids_are_opaque_and_stable_when_parsed() {
        let first = ProfileId::generate();
        let second = ProfileId::generate();
        assert_ne!(first, second);
        assert_eq!(ProfileId::parse(first.to_string()).unwrap(), first);
        assert_eq!(first.as_str().len(), 32);
    }

    #[test]
    fn endpoint_storage_keys_do_not_contain_ssh_targets() {
        let profile = ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(
            ClientEndpointId::Ssh(profile).storage_key(),
            "ssh:0123456789abcdef0123456789abcdef"
        );
    }
}
