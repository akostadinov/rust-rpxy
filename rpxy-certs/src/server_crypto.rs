use crate::{certs::SingleServerCertsKeys, error::*, log::*};
use rustc_hash::FxHashMap as HashMap;
use rustls::{
  server::{ResolvesServerCertUsingSni, WebPkiClientVerifier},
  RootCertStore, ServerConfig,
};
use std::sync::Arc;

/* ------------------------------------------------ */
/// ServerName in bytes type (TODO: this may be changed to define `common` layer defining types of names. or should be independent?)
pub type ServerNameBytes = Vec<u8>;
/// Convert ServerName in bytes to string
fn server_name_bytes_to_string(server_name_bytes: &ServerNameBytes) -> Result<String, RpxyCertError> {
  let server_name = String::from_utf8(server_name_bytes.to_ascii_lowercase())?;
  Ok(server_name)
}

/// ServerName (SNI) to ServerConfig map type
pub type ServerNameCryptoMap = HashMap<ServerNameBytes, Arc<ServerConfig>>;

/// ServerName (SNI) to ServerConfig map
pub struct ServerCrypto {
  // For Quic/HTTP3, only servers with no client authentication, aggregated server config
  pub aggregated_config_no_client_auth: Arc<ServerConfig>,
  // For TLS over TCP/HTTP2 and 1.1, map of SNI to server_crypto for all given servers
  pub individual_config_map: Arc<ServerNameCryptoMap>,
}

/* ------------------------------------------------ */
/// Reloader target for the certificate reloader service
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct ServerCryptoBase {
  /// Map of server name to certs and keys
  pub(super) inner: HashMap<ServerNameBytes, SingleServerCertsKeys>,
}

impl TryInto<Arc<ServerCrypto>> for &ServerCryptoBase {
  type Error = RpxyCertError;

  fn try_into(self) -> Result<Arc<ServerCrypto>, Self::Error> {
    let aggregated = self.build_aggrated_server_crypto()?;
    let individual = self.build_individual_server_crypto_map()?;

    Ok(Arc::new(ServerCrypto {
      aggregated_config_no_client_auth: Arc::new(aggregated),
      individual_config_map: Arc::new(individual),
    }))
  }
}

impl ServerCryptoBase {
  /// Build individual server crypto inner object
  fn build_individual_server_crypto_map(&self) -> Result<ServerNameCryptoMap, RpxyCertError> {
    let mut server_crypto_map: ServerNameCryptoMap = HashMap::default();

    for (server_name_bytes, certs_keys) in self.inner.iter() {
      let server_name = server_name_bytes_to_string(server_name_bytes)?;

      // Parse server certificates and private keys
      let Ok(certified_key) = certs_keys.rustls_certified_key() else {
        warn!("Failed to add certificate for {server_name}");
        continue;
      };

      let mut resolver_local = ResolvesServerCertUsingSni::new();
      if let Err(e) = resolver_local.add(&server_name, certified_key) {
        error!("{server_name}: Failed to read some certificates and keys {e}");
      };

      // With no client authentication case
      if !certs_keys.is_mutual_tls() {
        let mut server_crypto_local = ServerConfig::builder()
          .with_no_client_auth()
          .with_cert_resolver(Arc::new(resolver_local));
        #[cfg(feature = "http3")]
        {
          server_crypto_local.alpn_protocols = vec![b"h3".to_vec(), b"h2".to_vec(), b"http/1.1".to_vec()];
        }
        #[cfg(not(feature = "http3"))]
        {
          server_crypto_local.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        }
        server_crypto_map.insert(server_name_bytes.clone(), Arc::new(server_crypto_local));
        continue;
      }

      // With client authentication case, enable only http2 and http1.1
      let mut client_ca_roots_local = RootCertStore::empty();
      let Ok(trust_anchors) = certs_keys.rustls_client_certs_trust_anchors() else {
        warn!("Failed to add client CA certificate for {server_name}");
        continue;
      };
      let trust_anchors_without_skid = trust_anchors.values().map(|ta| ta.to_owned());
      client_ca_roots_local.extend(trust_anchors_without_skid);

      let Ok(client_cert_verifier) = WebPkiClientVerifier::builder(Arc::new(client_ca_roots_local)).build() else {
        warn!("Failed to build client CA certificate verifier for {server_name}");
        continue;
      };
      let mut server_crypto_local = ServerConfig::builder()
        .with_client_cert_verifier(client_cert_verifier)
        .with_cert_resolver(Arc::new(resolver_local));
      server_crypto_local.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
      server_crypto_map.insert(server_name_bytes.clone(), Arc::new(server_crypto_local));
    }

    Ok(server_crypto_map)
  }

  /* ------------------------------------------------ */
  /// Build aggregated server crypto inner object for no client auth server especially for http3
  fn build_aggrated_server_crypto(&self) -> Result<ServerConfig, RpxyCertError> {
    let mut resolver_global = ResolvesServerCertUsingSni::new();

    for (server_name_bytes, certs_keys) in self.inner.iter() {
      let server_name = server_name_bytes_to_string(server_name_bytes)?;

      // Parse server certificates and private keys
      let Ok(certified_key) = certs_keys.rustls_certified_key() else {
        warn!("Failed to add certificate for {server_name}");
        continue;
      };
      // Add server certificates and private keys to resolver only if client CA certs are not present
      if !certs_keys.is_mutual_tls() {
        // aggregated server config for no client auth server for http3
        if let Err(e) = resolver_global.add(&server_name, certified_key) {
          error!("{server_name}: Failed to read some certificates and keys {e}");
        };
      }
    }

    let mut server_crypto_global = ServerConfig::builder()
      .with_no_client_auth()
      .with_cert_resolver(Arc::new(resolver_global));

    #[cfg(feature = "http3")]
    {
      server_crypto_global.alpn_protocols = vec![b"h3".to_vec(), b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    #[cfg(not(feature = "http3"))]
    {
      server_crypto_global.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }

    Ok(server_crypto_global)
  }
}
