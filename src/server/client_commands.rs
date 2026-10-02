use std::io;
use std::sync::mpsc;

use tokio::sync::mpsc as tokio_mpsc;

use crate::api::schema::{ErrorBody, ErrorResponse, Method};

use super::client_transport::ServerEvent;

pub(crate) const MAX_ENDPOINT_COMMAND_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_ENDPOINT_BOOT_ID_BYTES: usize = 128;
pub(crate) const MAX_ENDPOINT_REQUEST_ID_BYTES: usize = 128;
const ENDPOINT_RESPONSE_CHUNK_BYTES: usize = 512 * 1024;

const CLIENT_SHELL_METHODS: &[&str] = &[
    "agent.activate",
    "agent.notice_dismiss",
    "agent.restart",
    "agent.suspend",
    "agent.transcripts",
    "agents.fix",
    "agents.settings",
    "agents.settings.set",
    "browser.fix",
    "browser.focus",
    "browser.get",
    "browser.settings",
    "browser.settings.set",
    "browser.start",
    "browser.stop",
    "checkpoints.add",
    "checkpoints.context",
    "checkpoints.list",
    "checkpoints.remove",
    "checkpoints.update",
    "client_shell.surface.set",
    "command.invoke",
    "coordinator.get",
    "coordinator.open",
    "coordinator.open_dashboard",
    "coordinator.set_enabled",
    "coordinator.set_model",
    "coordinator.set_notify",
    "coordinator.set_wake_caps",
    "coordinator.start",
    "coordinator.wake",
    "integration.install",
    "integration.list",
    "layout.set_split_ratio",
    "news.get",
    "news.open",
    "news.run",
    "news.set_enabled",
    "news.set_quiet_hours",
    "news.set_times",
    "notes.append",
    "notes.get",
    "notes.set",
    "pane.clear",
    "pane.close",
    "pane.copy_motion",
    "pane.copy_search",
    "pane.edit_scrollback",
    "pane.focus",
    "pane.focus_direction",
    "pane.input.set",
    "pane.link.activate",
    "pane.link.resolve",
    "pane.move",
    "pane.rename",
    "pane.resize",
    "pane.scroll",
    "pane.selection.read",
    "pane.split",
    "pane.swap",
    "pane.zoom",
    "product_announcement.dismiss",
    "release_notes.dismiss",
    "server.reload_config",
    "session.closed_list",
    "session.closed_remove",
    "session.closed_reopen",
    "tab.close",
    "tab.create",
    "tab.focus",
    "tab.move",
    "tab.rename",
    "tab.set_color",
    "tab.set_remind",
    "tab.set_reminder",
    "team.disband",
    "team.get",
    "team.join",
    "team.leave",
    "team.make",
    "team.set_purpose",
    "team.set_role",
    "workspace.close",
    "workspace.create",
    "workspace.focus",
    "workspace.move",
    "workspace.move_block",
    "workspace.rename",
    "worktree.create",
    "worktree.list",
    "worktree.open",
    "worktree.remove",
];

pub(crate) fn supported_client_shell_method_names() -> &'static [&'static str] {
    CLIENT_SHELL_METHODS
}

pub(crate) fn supports_client_shell_method_name(method: &str) -> bool {
    CLIENT_SHELL_METHODS.contains(&method)
}

pub(crate) fn supports_client_shell_method(method: &Method) -> bool {
    supports_client_shell_method_name(crate::api::api_method_name(method))
}

pub(crate) fn error_response(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"serialization_error","message":"failed to serialize endpoint response"}}"#.into()
    })
}

pub(crate) fn success_message_with_result(
    boot_id: String,
    request_id: String,
    result: crate::api::schema::ResponseResult,
) -> crate::protocol::ServerMessage {
    let response = serde_json::to_string(&crate::api::schema::SuccessResponse {
        id: request_id.clone(),
        result,
    })
    .unwrap_or_else(|_| {
        error_response(
            request_id.clone(),
            "serialization_error",
            "failed to serialize endpoint response",
        )
    });
    crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk: true,
        data: response.into_bytes(),
    }
}

pub(crate) fn error_message(
    boot_id: String,
    request_id: String,
    code: &str,
    message: impl Into<String>,
) -> crate::protocol::ServerMessage {
    let response = error_response(request_id.clone(), code, message);
    crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
        boot_id,
        request_id,
        final_chunk: true,
        data: response.into_bytes(),
    }
}

fn correlate_response_id(response: String, request_id: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&response) else {
        return response;
    };
    let Some(id) = value.get_mut("id") else {
        return response;
    };
    if id.as_str() == Some(request_id) {
        return response;
    }
    *id = serde_json::Value::String(request_id.to_owned());
    serde_json::to_string(&value).unwrap_or(response)
}

pub(crate) fn spawn_response_waiter(
    client_id: u64,
    boot_id: String,
    request_id: String,
    response_rx: mpsc::Receiver<String>,
    server_event_tx: tokio_mpsc::Sender<ServerEvent>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("herdr-client-endpoint-response".into())
        .spawn(move || {
            let response = response_rx.recv().unwrap_or_else(|_| {
                error_response(
                    request_id.clone(),
                    "server_unavailable",
                    "endpoint command ended without a response",
                )
            });
            let response = correlate_response_id(response, &request_id).into_bytes();
            if response.is_empty() {
                let _ = server_event_tx.blocking_send(
                    ServerEvent::ClientShellEndpointResponseChunkReady {
                        client_id,
                        boot_id,
                        request_id,
                        final_chunk: true,
                        data: Vec::new(),
                    },
                );
                return;
            }
            let chunk_count = response.len().div_ceil(ENDPOINT_RESPONSE_CHUNK_BYTES);
            for (index, chunk) in response.chunks(ENDPOINT_RESPONSE_CHUNK_BYTES).enumerate() {
                if server_event_tx
                    .blocking_send(ServerEvent::ClientShellEndpointResponseChunkReady {
                        client_id,
                        boot_id: boot_id.clone(),
                        request_id: request_id.clone(),
                        final_chunk: index + 1 == chunk_count,
                        data: chunk.to_vec(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use sha2::{Digest, Sha256};

    use super::*;

    fn collect_schema_refs(value: &serde_json::Value, refs: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
                    if let Some(name) = reference.rsplit('/').next() {
                        refs.insert(name.to_owned());
                    }
                }
                for value in object.values() {
                    collect_schema_refs(value, refs);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    collect_schema_refs(value, refs);
                }
            }
            _ => {}
        }
    }

    fn normalized_wire_schema(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => serde_json::Value::Object(
                object
                    .iter()
                    .filter(|(key, _)| {
                        !matches!(
                            key.as_str(),
                            "description" | "examples" | "readOnly" | "title" | "writeOnly"
                        )
                    })
                    .map(|(key, value)| (key.clone(), normalized_wire_schema(value)))
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.iter().map(normalized_wire_schema).collect())
            }
            _ => value.clone(),
        }
    }

    fn endpoint_method_shape_digests() -> BTreeMap<String, String> {
        let schema = serde_json::to_value(schemars::schema_for!(crate::api::schema::Request))
            .expect("request schema");
        let definitions = schema
            .get("$defs")
            .and_then(serde_json::Value::as_object)
            .expect("request definitions");
        let branches = schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("request method branches");
        let mut digests = BTreeMap::new();

        for method in CLIENT_SHELL_METHODS {
            let branch = branches
                .iter()
                .find(|branch| {
                    branch
                        .pointer("/properties/method/const")
                        .and_then(serde_json::Value::as_str)
                        == Some(method)
                })
                .unwrap_or_else(|| panic!("missing request schema branch for {method}"));
            let mut referenced_names = BTreeSet::new();
            collect_schema_refs(branch, &mut referenced_names);
            let mut visited_names = BTreeSet::new();
            let mut selected_definitions = serde_json::Map::new();
            while let Some(name) = referenced_names.pop_first() {
                if !visited_names.insert(name.clone()) {
                    continue;
                }
                let definition = definitions
                    .get(&name)
                    .unwrap_or_else(|| panic!("missing schema definition {name} for {method}"));
                collect_schema_refs(definition, &mut referenced_names);
                selected_definitions.insert(name, normalized_wire_schema(definition));
            }
            let shape = serde_json::json!({
                "request": normalized_wire_schema(branch),
                "definitions": selected_definitions,
            });
            let bytes = serde_json::to_vec(&shape).expect("method shape json");
            digests.insert(method.to_string(), format!("{:x}", Sha256::digest(bytes)));
        }

        digests
    }

    #[test]
    fn advertised_client_shell_method_shapes_stay_at_the_v1_contract() {
        let expected: BTreeMap<String, String> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-method-shapes-v1.json"
        )))
        .expect("endpoint method shape fixture");
        let mut actual = endpoint_method_shape_digests();
        // Freeze additive methods separately without rewriting the published fixture.
        assert_eq!(
            actual.remove("pane.clear").as_deref(),
            Some("0301d288ba198ddaa427dd7421c71911cccaf4ea03544531efa8b67ca21b08f6")
        );
        assert_eq!(
            actual.remove("pane.link.resolve").as_deref(),
            Some("f5e4a3e01453ae7b188f127ce951c12c20e0bebcc17cc364eeb6d1a01fd5bf81")
        );
        assert_eq!(
            actual.remove("agent.suspend").as_deref(),
            Some("8f5505e98a84d17e84b6a0e6adfa519288db204f2cec400ef4ce24a7819c582c")
        );
        assert_eq!(
            actual.remove("agent.activate").as_deref(),
            Some("ab4e42fe98b4334c06bc65f0dd8ca9f6c9f7ec1c8da950d6df755d992ec4b06b")
        );
        assert_eq!(
            actual.remove("pane.move").as_deref(),
            Some("eaed63cf205db2dc043ecce9e1a79cdca7f6e2521b364226bbf3121affadce7c")
        );
        assert_eq!(
            actual.remove("agent.transcripts").as_deref(),
            Some("8f06825b38522b26205170bb58195c5401724155c18c878ca06555b9a4917cc8")
        );
        assert_eq!(
            actual.remove("agent.restart").as_deref(),
            Some("dc124dcfe9d7fc0fe3d9a85e00548a0263574d3de68ffd16370f2b6ba67ad062")
        );
        assert_eq!(
            actual.remove("tab.set_color").as_deref(),
            Some("47beb991c2a5f54fffc12c8bfc0a27c725487f94e3088474e80bc8526ced74c2")
        );
        assert_eq!(
            actual.remove("tab.set_remind").as_deref(),
            Some("e9c63bb9f29eacf951cfee82469449b61eb3b72ae80d0c0c9a3cb6319aa4bb29")
        );
        assert_eq!(
            actual.remove("tab.set_reminder").as_deref(),
            Some("597627419b2ba06f50d220dd1ea4218fbb2204ba3452bbc25897cdc417da5b1e")
        );
        assert_eq!(
            actual.remove("session.closed_list").as_deref(),
            Some("36061ddc74238ec4cbe0e9b01e685d417718a12f09ebac9334b7c1fc754c22cd")
        );
        assert_eq!(
            actual.remove("session.closed_remove").as_deref(),
            Some("9583e60d3393fbbcec1a209e4ce7b227ce38d429b0d0f1e9dac1ac2bb3befbdb")
        );
        assert_eq!(
            actual.remove("session.closed_reopen").as_deref(),
            Some("f18fab7830a2e3c6e915def7f93b9afb48f406bc78001cecd0a17f188291306d")
        );
        assert_eq!(
            actual.remove("news.get").as_deref(),
            Some("aa473a7480d449faed99b6e7958a359b79c42352fa2baf37a080c17d17c8c855")
        );
        assert_eq!(
            actual.remove("news.open").as_deref(),
            Some("bb5614fbd62d35a07127482009b8f69808325fecf27588dc70da6a19b15b4fb7")
        );
        assert_eq!(
            actual.remove("news.run").as_deref(),
            Some("19594c58ed9598a02c2edd64f939dfd11e71b82267bdcaac244a3f5e840209ff")
        );
        assert_eq!(
            actual.remove("news.set_enabled").as_deref(),
            Some("1246ec256f7160ad56a4bc171faceb2e68649e592628021b0f821624c87b2d74")
        );
        assert_eq!(
            actual.remove("news.set_times").as_deref(),
            Some("7962b61c93c83ef9e9cdd0b42a433ae171891fc11b1df42a5120417d9cb9d805")
        );
        assert_eq!(
            actual.remove("browser.focus").as_deref(),
            Some("8e5a572a95b7e84c5c9802ce81f6a18ff723ad2574247981627afd04383ab9a4")
        );
        assert_eq!(
            actual.remove("browser.get").as_deref(),
            Some("9957cef2770129f543c007b2ae28b61b960e7c8c060f287c0fe0cdbb64a08fb8")
        );
        assert_eq!(
            actual.remove("browser.start").as_deref(),
            Some("cfcaeeee2210fdf0d9425d462b0947b389be24d1357bbcdd39dd99608b150f51")
        );
        assert_eq!(
            actual.remove("browser.stop").as_deref(),
            Some("7b8ec002e4def703fdb73643a82bb087e2afa69f56629f371a4c79b56a631e41")
        );
        assert_eq!(
            actual.remove("news.set_quiet_hours").as_deref(),
            Some("985cacc65cb6e91faef3336f78ae66132ed67d8b2a815805876ea78be3682863")
        );
        assert_eq!(
            actual.remove("browser.settings").as_deref(),
            Some("3c10d3f5d952a2f78ea6536fc0730f87833b6bd41385ba2c9a208451bfff2cf8")
        );
        assert_eq!(
            actual.remove("browser.settings.set").as_deref(),
            Some("6460c5c56d7e1e241ed59c4d76174e7fc4b7ab03b53c50fed717d359fddea833")
        );
        assert_eq!(
            actual.remove("browser.fix").as_deref(),
            Some("71558d5a9cf80cc82952b6bc1330776bce901118546e242640bf2384f51f5a8c")
        );
        assert_eq!(
            actual.remove("coordinator.get").as_deref(),
            Some("d6ec446e6d7ee38199feb4f9c48198613e9f339a534012bde7160fb07a63eaa8")
        );
        assert_eq!(
            actual.remove("coordinator.open").as_deref(),
            Some("24a5c11d01bf8c6a65452115dc121a17f077ddf9d504d5c88c137f3b850536cc")
        );
        assert_eq!(
            actual.remove("coordinator.open_dashboard").as_deref(),
            Some("d3d061db09bc59874312f676504bed2a6cc127bf6da2738f7214e32fd87facc0")
        );
        assert_eq!(
            actual.remove("coordinator.set_enabled").as_deref(),
            Some("e7a703e3d1daa27cbe5b8637f7a3b8ac74a0c8f40497c88a53b699311d1bd71c")
        );
        assert_eq!(
            actual.remove("coordinator.set_model").as_deref(),
            Some("33c2821d6c15c99671f9df050f2f6caef09fc91c9724b70e22fcc92ad1996e7b")
        );
        assert_eq!(
            actual.remove("coordinator.set_notify").as_deref(),
            Some("78ad0eb2d53f3fe07c1dda181ede781ee94914dd7d33d037df56cdcd6e503126")
        );
        assert_eq!(
            actual.remove("coordinator.set_wake_caps").as_deref(),
            Some("9f6f9010ba7b4b9a8b8d8835d117f0ddd864f4184ee3324f50d0737fa084510a")
        );
        assert_eq!(
            actual.remove("coordinator.start").as_deref(),
            Some("d536a69f3c65a742c3ab09e46e3b2f28aaa44c0d1570aa75befb14fb2bf3482e")
        );
        assert_eq!(
            actual.remove("coordinator.wake").as_deref(),
            Some("9b8885f0516494756d440daf6de982049a3c0e7ea65b4f2395ff6446a2a62f94")
        );
        // fork: agent cards and the Agents settings section.
        assert_eq!(
            actual.remove("agent.notice_dismiss").as_deref(),
            Some("b82b1a70813888abb724380331ba44fb72812e9b177af4ba08cd17e104bba094")
        );
        assert_eq!(
            actual.remove("agents.fix").as_deref(),
            Some("5c28b30fb6a27e53ea9144fadfe971cc9ef8c7682fabe21cc99f5485ff250dd5")
        );
        assert_eq!(
            actual.remove("agents.settings").as_deref(),
            Some("bdae4ed7aed2040ce09b342c53c7e0ca69882ca001d12f7a61f7127fbd2a5b6a")
        );
        assert_eq!(
            actual.remove("agents.settings.set").as_deref(),
            Some("1b012b44b0d82bfa63daaf37373d75b76289217c1677345bfd4dc8c27c2b59d0")
        );
        // fork: teams.
        assert_eq!(
            actual.remove("team.disband").as_deref(),
            Some("967a5429a196186df1bbfd4a382205220640fd473608296a690f6cc75e8bdf16")
        );
        assert_eq!(
            actual.remove("team.get").as_deref(),
            Some("c1f9494dc09f9a6f0284aae2c3c349d3599e12353606abc25f56c5eac9d52345")
        );
        assert_eq!(
            actual.remove("team.join").as_deref(),
            Some("a3f0ed909e8e9b17835308e4b416d4d4bbb746f3317d9cdd088396fe41853094")
        );
        assert_eq!(
            actual.remove("team.leave").as_deref(),
            Some("db3e86aca6a94e80e07238ad8d3ecb88997f6ec95524d4c45586b212b9eeaf2b")
        );
        assert_eq!(
            actual.remove("team.make").as_deref(),
            Some("51ce432dd19278c5258f1dc07a4b21261bf2788fd8de0d319cd75d19ffa2562c")
        );
        assert_eq!(
            actual.remove("team.set_purpose").as_deref(),
            Some("9e3683747ca882cfa21e6200cc814a9d7c1ce91a90e5e68e3b548a7aec32f89f")
        );
        assert_eq!(
            actual.remove("team.set_role").as_deref(),
            Some("0d0663aa876a6f81790a20e944cfd655a045a3427edde7ffe13fb74da6cd29f3")
        );
        assert_eq!(
            actual.remove("notes.get").as_deref(),
            Some("514adb7872ed9fe98237146f87911e1f00d4fb3fb3d75e4054c783fc23e848bc")
        );
        assert_eq!(
            actual.remove("notes.set").as_deref(),
            Some("1ae65a5c7d8087a5e95169878a81ccd8bb532255d22f438fadc0ee2fb428afdb")
        );
        assert_eq!(
            actual.remove("notes.append").as_deref(),
            Some("00925fe4941e3de5b418a568645b880f65e5c24c33f12b1b820a018f36555b51")
        );
        assert_eq!(
            actual.remove("checkpoints.list").as_deref(),
            Some("df0d37ec4550faaef6d7cd032f75aeac6403b70415a293d29a24f1c458f04ce2")
        );
        assert_eq!(
            actual.remove("checkpoints.add").as_deref(),
            Some("f7e43fa3c09ace07c80fcd4b06e0e47d8df8d83eacbb76613ad17cd881deb580")
        );
        assert_eq!(
            actual.remove("checkpoints.update").as_deref(),
            Some("a563335e063a6aa7a277b71b745b3b2cf6695efeae5457e9ff44130aba72b098")
        );
        assert_eq!(
            actual.remove("checkpoints.remove").as_deref(),
            Some("f510c7c9e04c24b347efd4ea37ac98bb11016f1654816f35dab5d455ff329248")
        );
        assert_eq!(
            actual.remove("checkpoints.context").as_deref(),
            Some("bce19edc6742c1a9c961a4bee33d9c1b5d0c516a03e65d0ca5ab722b6da47d93")
        );

        assert_eq!(
            actual, expected,
            "an existing endpoint method changed shape; add load-bearing behavior as a new advertised method or explicitly gate new fields"
        );
    }

    #[test]
    fn every_notes_method_is_advertised_to_client_shells() {
        for method in crate::api::schema::notes::method::ALL {
            assert!(
                supports_client_shell_method_name(method),
                "{method} is not in CLIENT_SHELL_METHODS"
            );
        }
    }

    #[test]
    fn team_methods_are_advertised_except_the_list_and_the_agent_read() {
        for method in crate::api::schema::team::method::CLIENT_SHELL {
            assert!(
                supports_client_shell_method_name(method),
                "{method} is not in CLIENT_SHELL_METHODS"
            );
        }
        assert!(!supports_client_shell_method_name(
            crate::api::schema::team::method::LIST
        ));
        assert!(!supports_client_shell_method_name(
            crate::api::schema::team::method::CONTEXT
        ));
        assert_eq!(CLIENT_SHELL_METHODS.len(), 92);
    }

    #[test]
    fn team_params_reference_no_existing_schema_type() {
        // Digest hygiene: a team request branch references only team
        // parameter structs, so no other schema change can move its digest.
        let schema = serde_json::to_value(schemars::schema_for!(crate::api::schema::Request))
            .expect("request schema");
        let definitions = schema
            .get("$defs")
            .and_then(serde_json::Value::as_object)
            .expect("request definitions");
        let branches = schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("request method branches");
        for method in crate::api::schema::team::method::ALL {
            let branch = branches
                .iter()
                .find(|branch| {
                    branch
                        .pointer("/properties/method/const")
                        .and_then(serde_json::Value::as_str)
                        == Some(method)
                })
                .unwrap_or_else(|| panic!("missing request schema branch for {method}"));
            let mut referenced = BTreeSet::new();
            collect_schema_refs(branch, &mut referenced);
            let mut visited = BTreeSet::new();
            while let Some(name) = referenced.pop_first() {
                if !visited.insert(name.clone()) {
                    continue;
                }
                assert!(
                    name.starts_with("Team") || name == "EmptyParams",
                    "{method} references the existing type {name}"
                );
                if let Some(definition) = definitions.get(&name) {
                    collect_schema_refs(definition, &mut referenced);
                }
            }
        }
    }

    #[test]
    fn every_coordinator_method_is_advertised_to_client_shells() {
        for method in crate::api::schema::coordinator::method::ALL {
            assert!(
                supports_client_shell_method_name(method),
                "{method} is not in CLIENT_SHELL_METHODS"
            );
        }
    }

    #[test]
    fn advertised_client_shell_methods_are_sorted_unique_and_in_schema() {
        assert!(CLIENT_SHELL_METHODS
            .windows(2)
            .all(|pair| pair[0] < pair[1]));

        fn collect_method_constants(value: &serde_json::Value, methods: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(method) = object
                        .get("const")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| value.contains('.'))
                    {
                        methods.push(method.to_owned());
                    }
                    for value in object.values() {
                        collect_method_constants(value, methods);
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        collect_method_constants(value, methods);
                    }
                }
                _ => {}
            }
        }

        let schema = serde_json::to_value(schemars::schema_for!(crate::api::schema::Request))
            .expect("request schema");
        let mut schema_methods = Vec::new();
        collect_method_constants(&schema, &mut schema_methods);
        for method in CLIENT_SHELL_METHODS {
            assert!(
                schema_methods.iter().any(|candidate| candidate == method),
                "advertised endpoint method {method:?} is absent from the request schema"
            );
        }
    }

    #[test]
    fn client_shell_lane_excludes_api_front_door_and_lifecycle_methods() {
        assert!(supports_client_shell_method(
            &Method::ClientShellSurfaceSet(crate::api::schema::ClientShellSurfaceSetParams {
                active: false,
            })
        ));
        assert!(supports_client_shell_method(&Method::ServerReloadConfig(
            crate::api::schema::EmptyParams::default(),
        )));
        assert!(supports_client_shell_method(&Method::PaneLinkActivate(
            crate::api::schema::PaneLinkActivateParams {
                pane_id: "w1:p1".into(),
                viewport_row: 0,
                col: 0,
                content_revision: None,
                offset_from_bottom: None,
            },
        )));
        assert!(supports_client_shell_method(&Method::PaneLinkResolve(
            crate::api::schema::PaneLinkActivateParams {
                pane_id: "w1:p1".into(),
                viewport_row: 0,
                col: 0,
                content_revision: None,
                offset_from_bottom: None,
            },
        )));
        assert!(!supports_client_shell_method(&Method::Ping(
            crate::api::schema::PingParams::default(),
        )));
        assert!(!supports_client_shell_method(&Method::ServerStop(
            crate::api::schema::EmptyParams::default(),
        )));
    }

    #[test]
    fn endpoint_response_uses_the_client_request_id() {
        let response = serde_json::json!({
            "id": "endpoint:boot-a:7:client-shell:1",
            "result": { "type": "ok" }
        })
        .to_string();

        let correlated = correlate_response_id(response, "client-shell:1");
        let decoded: serde_json::Value = serde_json::from_str(&correlated).expect("response json");

        assert_eq!(decoded["id"], "client-shell:1");
    }

    #[test]
    fn endpoint_responses_are_chunked_without_truncation() {
        let (response_tx, response_rx) = mpsc::channel();
        let (event_tx, mut event_rx) = tokio_mpsc::channel(8);
        spawn_response_waiter(
            7,
            "boot-a".into(),
            "request-a".into(),
            response_rx,
            event_tx,
        )
        .unwrap();
        let response = "x".repeat(ENDPOINT_RESPONSE_CHUNK_BYTES + 17);
        response_tx.send(response.clone()).unwrap();

        let mut received = Vec::new();
        loop {
            let ServerEvent::ClientShellEndpointResponseChunkReady {
                client_id,
                boot_id,
                request_id,
                final_chunk,
                data,
            } = event_rx.blocking_recv().expect("response chunk")
            else {
                panic!("expected response chunk");
            };
            assert_eq!(client_id, 7);
            assert_eq!(boot_id, "boot-a");
            assert_eq!(request_id, "request-a");
            received.extend(data);
            if final_chunk {
                break;
            }
        }

        assert_eq!(received, response.as_bytes());
    }
}
