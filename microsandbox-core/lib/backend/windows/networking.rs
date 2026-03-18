//! Windows networking via HCN (Host Compute Network).
//!
//! Manages a shared NAT network, per-sandbox HCN endpoints, and per-port
//! HCN load balancers for port forwarding. DNS is forwarded via the NAT
//! gateway IP.

use serde::{Deserialize, Serialize};

use crate::{MicrosandboxError, MicrosandboxResult};

//--------------------------------------------------------------------------------------------------
// Constants
//--------------------------------------------------------------------------------------------------

/// Name of the shared microsandbox NAT network.
const NAT_NETWORK_NAME: &str = "microsandbox-nat";

/// NAT subnet for sandbox networking.
const NAT_SUBNET: &str = "172.28.0.0/16";

/// NAT gateway IP (used as DNS forwarder).
const NAT_GATEWAY_IP: &str = "172.28.176.1";

//--------------------------------------------------------------------------------------------------
// Types
//--------------------------------------------------------------------------------------------------

/// Represents the shared NAT network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatNetwork {
    /// HCN network ID.
    pub id: String,

    /// Network name.
    pub name: String,

    /// NAT subnet (CIDR notation).
    pub subnet: String,

    /// Gateway IP address.
    pub gateway_ip: String,
}

/// Represents a per-sandbox HCN endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcnEndpoint {
    /// HCN endpoint ID.
    pub id: String,

    /// The sandbox this endpoint belongs to.
    pub sandbox_key: String,

    /// IP address assigned to this endpoint.
    pub ip_address: String,

    /// The network this endpoint is attached to.
    pub network_id: String,
}

/// Represents an HCN load balancer for port forwarding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcnLoadBalancer {
    /// HCN load balancer ID.
    pub id: String,

    /// Host port (bound to 127.0.0.1).
    pub host_port: u16,

    /// Guest port (forwarded to sandbox IP).
    pub guest_port: u16,

    /// Endpoint IDs this load balancer forwards to.
    pub endpoint_ids: Vec<String>,
}

/// Manages HCN networking for Windows sandboxes.
#[derive(Debug)]
pub struct HcnNetworkManager {
    /// The shared NAT network (created on first use).
    nat_network: Option<NatNetwork>,
}

//--------------------------------------------------------------------------------------------------
// Methods
//--------------------------------------------------------------------------------------------------

impl HcnNetworkManager {
    /// Creates a new `HcnNetworkManager`.
    pub fn new() -> Self {
        Self { nat_network: None }
    }

    /// Returns the NAT gateway IP address (used as DNS forwarder for guests).
    pub fn gateway_ip() -> &'static str {
        NAT_GATEWAY_IP
    }

    /// Returns the NAT subnet.
    pub fn subnet() -> &'static str {
        NAT_SUBNET
    }

    /// Ensures the shared NAT network exists, creating it if necessary.
    ///
    /// On first call, creates the HCN NAT network. On subsequent calls or
    /// server restarts, detects and reuses the existing network.
    pub async fn ensure_nat_network(&mut self) -> MicrosandboxResult<&NatNetwork> {
        if self.nat_network.is_some() {
            return Ok(self.nat_network.as_ref().unwrap());
        }

        // Try to find an existing network first.
        let existing = Self::find_existing_network().await?;
        if let Some(network) = existing {
            tracing::info!(
                id = %network.id,
                "Reusing existing NAT network"
            );
            self.nat_network = Some(network);
            return Ok(self.nat_network.as_ref().unwrap());
        }

        // Create a new NAT network via PowerShell/HCN.
        tracing::info!("Creating shared NAT network '{}'", NAT_NETWORK_NAME);

        let create_script = format!(
            r#"
$network = New-HnsNetwork -Name '{}' -Type NAT -AddressPrefix '{}' -Gateway '{}'
$network.Id
"#,
            NAT_NETWORK_NAME, NAT_SUBNET, NAT_GATEWAY_IP
        );

        let output = tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &create_script])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!(
                    "Failed to create HCN NAT network: {}",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MicrosandboxError::NotImplemented(format!(
                "Failed to create NAT network: {}",
                stderr
            )));
        }

        let network_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let network = NatNetwork {
            id: network_id,
            name: NAT_NETWORK_NAME.to_string(),
            subnet: NAT_SUBNET.to_string(),
            gateway_ip: NAT_GATEWAY_IP.to_string(),
        };

        self.nat_network = Some(network);
        Ok(self.nat_network.as_ref().unwrap())
    }

    /// Creates an HCN endpoint for a sandbox, attached to the NAT network.
    pub async fn create_endpoint(&self, sandbox_key: &str) -> MicrosandboxResult<HcnEndpoint> {
        let network = self.nat_network.as_ref().ok_or_else(|| {
            MicrosandboxError::NotImplemented(
                "NAT network not initialized. Call ensure_nat_network() first.".to_string(),
            )
        })?;

        let create_script = format!(
            r#"
$endpoint = New-HnsEndpoint -NetworkId '{}' -Name 'msb-{}'
@{{ Id = $endpoint.Id; IpAddress = $endpoint.IPAddress }} | ConvertTo-Json
"#,
            network.id, sandbox_key
        );

        let output = tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &create_script])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!("Failed to create HCN endpoint: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MicrosandboxError::NotImplemented(format!(
                "Failed to create endpoint for sandbox '{}': {}",
                sandbox_key, stderr
            )));
        }

        #[derive(Deserialize)]
        struct EndpointResult {
            #[serde(alias = "Id")]
            id: String,
            #[serde(alias = "IpAddress")]
            ip_address: String,
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: EndpointResult = serde_json::from_str(&stdout).map_err(|e| {
            MicrosandboxError::NotImplemented(format!("Failed to parse endpoint result: {}", e))
        })?;

        let endpoint = HcnEndpoint {
            id: result.id,
            sandbox_key: sandbox_key.to_string(),
            ip_address: result.ip_address,
            network_id: network.id.clone(),
        };

        tracing::info!(
            endpoint_id = %endpoint.id,
            ip = %endpoint.ip_address,
            sandbox = %sandbox_key,
            "Created HCN endpoint"
        );

        Ok(endpoint)
    }

    /// Creates an HCN load balancer for a port mapping.
    pub async fn create_load_balancer(
        &self,
        host_port: u16,
        guest_port: u16,
        endpoint_id: &str,
    ) -> MicrosandboxResult<HcnLoadBalancer> {
        let create_script = format!(
            r#"
$lb = New-HnsLoadBalancer -Endpoints @('{}') -InternalPort {} -ExternalPort {} -Vip '127.0.0.1'
$lb.Id
"#,
            endpoint_id, guest_port, host_port
        );

        let output = tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &create_script])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!(
                    "Failed to create HCN load balancer: {}",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MicrosandboxError::NotImplemented(format!(
                "Failed to create load balancer for port {}:{}: {}",
                host_port, guest_port, stderr
            )));
        }

        let lb_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let lb = HcnLoadBalancer {
            id: lb_id,
            host_port,
            guest_port,
            endpoint_ids: vec![endpoint_id.to_string()],
        };

        tracing::info!(
            lb_id = %lb.id,
            host_port = host_port,
            guest_port = guest_port,
            "Created HCN load balancer"
        );

        Ok(lb)
    }

    /// Deletes an HCN endpoint.
    pub async fn delete_endpoint(&self, endpoint_id: &str) -> MicrosandboxResult<()> {
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Remove-HnsEndpoint -Id '{}'", endpoint_id),
            ])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!("Failed to delete HCN endpoint: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                endpoint_id = %endpoint_id,
                error = %stderr,
                "Failed to delete HCN endpoint (may already be deleted)"
            );
        } else {
            tracing::debug!(endpoint_id = %endpoint_id, "Deleted HCN endpoint");
        }

        Ok(())
    }

    /// Deletes an HCN load balancer.
    pub async fn delete_load_balancer(&self, lb_id: &str) -> MicrosandboxResult<()> {
        let output = tokio::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Remove-HnsLoadBalancer -Id '{}'", lb_id),
            ])
            .output()
            .await
            .map_err(|e| {
                MicrosandboxError::NotImplemented(format!(
                    "Failed to delete HCN load balancer: {}",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                lb_id = %lb_id,
                error = %stderr,
                "Failed to delete HCN load balancer (may already be deleted)"
            );
        } else {
            tracing::debug!(lb_id = %lb_id, "Deleted HCN load balancer");
        }

        Ok(())
    }

    /// Cleans up all endpoints and load balancers for a sandbox.
    pub async fn cleanup_sandbox_networking(
        &self,
        endpoint: &HcnEndpoint,
        load_balancers: &[HcnLoadBalancer],
    ) -> MicrosandboxResult<()> {
        // Delete load balancers first.
        for lb in load_balancers {
            self.delete_load_balancer(&lb.id).await?;
        }

        // Then delete the endpoint.
        self.delete_endpoint(&endpoint.id).await?;

        Ok(())
    }

    /// Cleans up all managed networking resources on server shutdown.
    /// Preserves the NAT network for reuse on next start.
    pub async fn cleanup_all(
        &self,
        endpoints: &[HcnEndpoint],
        load_balancers: &[HcnLoadBalancer],
    ) -> MicrosandboxResult<()> {
        for lb in load_balancers {
            let _ = self.delete_load_balancer(&lb.id).await;
        }

        for endpoint in endpoints {
            let _ = self.delete_endpoint(&endpoint.id).await;
        }

        tracing::info!("Cleaned up all networking resources (NAT network preserved)");

        Ok(())
    }

    /// Tries to find an existing microsandbox NAT network.
    async fn find_existing_network() -> MicrosandboxResult<Option<NatNetwork>> {
        let find_script = format!(
            r#"
$net = Get-HnsNetwork | Where-Object {{ $_.Name -eq '{}' }}
if ($net) {{
    @{{ Id = $net.Id; Name = $net.Name; Subnet = '{}'; Gateway = '{}' }} | ConvertTo-Json
}} else {{
    'null'
}}
"#,
            NAT_NETWORK_NAME, NAT_SUBNET, NAT_GATEWAY_IP
        );

        let output = tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", &find_script])
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let trimmed = stdout.trim();

                if trimmed == "null" || trimmed.is_empty() {
                    return Ok(None);
                }

                #[derive(Deserialize)]
                struct NetworkResult {
                    #[serde(alias = "Id")]
                    id: String,
                    #[serde(alias = "Name")]
                    name: String,
                    #[serde(alias = "Subnet")]
                    subnet: String,
                    #[serde(alias = "Gateway")]
                    gateway: String,
                }

                let result: NetworkResult = serde_json::from_str(trimmed).map_err(|e| {
                    MicrosandboxError::NotImplemented(format!(
                        "Failed to parse network result: {}",
                        e
                    ))
                })?;

                Ok(Some(NatNetwork {
                    id: result.id,
                    name: result.name,
                    subnet: result.subnet,
                    gateway_ip: result.gateway,
                }))
            }
            _ => Ok(None),
        }
    }
}

impl Default for HcnNetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

//--------------------------------------------------------------------------------------------------
// Tests
//--------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nat_constants() {
        assert_eq!(NAT_NETWORK_NAME, "microsandbox-nat");
        assert_eq!(NAT_SUBNET, "172.28.0.0/16");
        assert_eq!(NAT_GATEWAY_IP, "172.28.176.1");
    }

    #[test]
    fn test_gateway_ip() {
        assert_eq!(HcnNetworkManager::gateway_ip(), "172.28.176.1");
    }

    #[test]
    fn test_nat_network_serialization() {
        let network = NatNetwork {
            id: "abc-123".to_string(),
            name: "microsandbox-nat".to_string(),
            subnet: "172.28.0.0/16".to_string(),
            gateway_ip: "172.28.176.1".to_string(),
        };

        let json = serde_json::to_string(&network).unwrap();
        let deserialized: NatNetwork = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "microsandbox-nat");
        assert_eq!(deserialized.gateway_ip, "172.28.176.1");
    }

    #[test]
    fn test_endpoint_serialization() {
        let endpoint = HcnEndpoint {
            id: "ep-123".to_string(),
            sandbox_key: "project~sandbox1".to_string(),
            ip_address: "172.28.0.2".to_string(),
            network_id: "net-456".to_string(),
        };

        let json = serde_json::to_string(&endpoint).unwrap();
        let deserialized: HcnEndpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.sandbox_key, "project~sandbox1");
        assert_eq!(deserialized.ip_address, "172.28.0.2");
    }

    #[test]
    fn test_load_balancer_serialization() {
        let lb = HcnLoadBalancer {
            id: "lb-789".to_string(),
            host_port: 8080,
            guest_port: 80,
            endpoint_ids: vec!["ep-123".to_string()],
        };

        let json = serde_json::to_string(&lb).unwrap();
        let deserialized: HcnLoadBalancer = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.host_port, 8080);
        assert_eq!(deserialized.guest_port, 80);
        assert_eq!(deserialized.endpoint_ids.len(), 1);
    }

    #[test]
    fn test_network_manager_default() {
        let manager = HcnNetworkManager::default();
        assert!(manager.nat_network.is_none());
    }
}
