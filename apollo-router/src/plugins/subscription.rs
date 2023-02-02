use std::str::FromStr;
use std::task::Context;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use http::StatusCode;
use multimap::MultiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::Value;
use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

use crate::graphql::Response;
use crate::notification::Notify;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::router;
use crate::Endpoint;
use crate::ListenAddr;

#[derive(Debug, Clone)]
struct Subscription {
    enabled: bool,
    notify: Notify,
}

/// Forbid mutations configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, default)]
struct SubscriptionConfig {
    enabled: bool,
    mode: SubscriptionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SubscriptionMode {
    // TODO add listen and path conf
    /// Using a callback url
    #[serde(rename = "callback")]
    Callback { public_url: String },
    /// Using websocket to directly connect to subgraph
    #[serde(rename = "passthrough")]
    Passthrough,
}

impl Default for SubscriptionMode {
    fn default() -> Self {
        // TODO change this default ?
        Self::Passthrough
    }
}

fn default_listen_addr() -> ListenAddr {
    ListenAddr::SocketAddr("127.0.0.1:4000".parse().expect("valid ListenAddr"))
}

#[async_trait::async_trait]
impl Plugin for Subscription {
    type Config = SubscriptionConfig;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Subscription {
            enabled: true,
            notify: init.notify,
        })
    }

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint> {
        let mut map = MultiMap::new();

        if self.enabled {
            let endpoint = Endpoint::from_router_service(
                String::from("/callback/:callback"),
                CallbackService::new(self.notify.clone()).boxed(),
            );
            map.insert(default_listen_addr(), endpoint);
        }

        map
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "kind", rename = "lowercase")]
enum CallbackPayload {
    #[serde(rename = "subscription")]
    Subscription { data: Response },
}

#[derive(Clone)]
pub(crate) struct CallbackService {
    notify: Notify,
}

impl CallbackService {
    pub(crate) fn new(notify: Notify) -> Self {
        Self { notify }
    }
}

impl Service<router::Request> for CallbackService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        let mut notify = self.notify.clone();
        Box::pin(async move {
            let (parts, body) = req.router_request.into_parts();
            let sub_id =
                match uuid::Uuid::from_str(parts.uri.path().trim_start_matches("/callback/")) {
                    Ok(sub_id) => sub_id,
                    Err(_) => {
                        return Ok(router::Response {
                            response: http::Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .body::<hyper::Body>("cannot convert the subscription id".into())
                                .map_err(BoxError::from)?,
                            context: req.context,
                        });
                    }
                };

            let cb_body = hyper::body::to_bytes(body)
                .await
                .map_err(|e| format!("failed to get the request body: {}", e))
                .and_then(|bytes| {
                    serde_json::from_reader::<_, CallbackPayload>(bytes.reader()).map_err(|err| {
                        format!("failed to deserialize the request body into JSON: {}", err)
                    })
                });
            let cb_body = match cb_body {
                Ok(cb_body) => cb_body,
                Err(err) => {
                    return Ok(router::Response {
                        response: http::Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .body(err.into())
                            .map_err(BoxError::from)?,
                        context: req.context,
                    });
                }
            };

            match cb_body {
                CallbackPayload::Subscription { data } => {
                    let mut handle = match notify.subscribe_if_exist(sub_id).await {
                        Some(handle) => handle,
                        None => {
                            return Ok(router::Response {
                                response: http::Response::builder()
                                    .status(StatusCode::NOT_FOUND)
                                    .body("suscription doesn't exist".into())
                                    .map_err(BoxError::from)?,
                                context: req.context,
                            });
                        }
                    };

                    handle.publish(sub_id, data).await;
                }
            }

            Ok(router::Response {
                response: http::Response::builder()
                    .status(StatusCode::OK)
                    .body::<hyper::Body>("ok".into())
                    .map_err(BoxError::from)?,
                context: req.context,
            })
        })
    }
}

// #[cfg(test)]
// mod tests {
//     use http::Method;
//     use http::StatusCode;
//     use serde_json::json;
//     use tower::ServiceExt;

//     use super::*;
//     use crate::graphql;
//     use crate::graphql::Response;
//     use crate::http_ext::Request;
//     use crate::plugin::test::MockExecutionService;
//     use crate::plugin::PluginInit;
//     use crate::query_planner::fetch::OperationKind;
//     use crate::query_planner::PlanNode;
//     use crate::query_planner::QueryPlan;

//     #[tokio::test]
//     async fn it_lets_queries_pass_through() {
//         let mut mock_service = MockExecutionService::new();

//         mock_service
//             .expect_call()
//             .times(1)
//             .returning(move |_| Ok(ExecutionResponse::fake_builder().build().unwrap()));

//         let service_stack = ForbidMutations::new(PluginInit::new(
//             ForbidMutationsConfig(true),
//             Default::default(),
//         ))
//         .await
//         .expect("couldn't create forbid_mutations plugin")
//         .execution_service(mock_service.boxed());

//         let request = create_request(Method::GET, OperationKind::Query);

//         let _ = service_stack
//             .oneshot(request)
//             .await
//             .unwrap()
//             .next_response()
//             .await
//             .unwrap();
//     }

//     #[tokio::test]
//     async fn it_doesnt_let_mutations_pass_through() {
//         let expected_error = Error::builder()
//             .message("Mutations are forbidden".to_string())
//             .extension_code("MUTATION_FORBIDDEN")
//             .build();
//         let expected_status = StatusCode::BAD_REQUEST;

//         let service_stack = ForbidMutations::new(PluginInit::new(
//             ForbidMutationsConfig(true),
//             Default::default(),
//         ))
//         .await
//         .expect("couldn't create forbid_mutations plugin")
//         .execution_service(MockExecutionService::new().boxed());
//         let request = create_request(Method::GET, OperationKind::Mutation);

//         let mut actual_error = service_stack.oneshot(request).await.unwrap();

//         assert_eq!(expected_status, actual_error.response.status());
//         assert_error_matches(&expected_error, actual_error.next_response().await.unwrap());
//     }

//     #[tokio::test]
//     async fn configuration_set_to_false_lets_mutations_pass_through() {
//         let mut mock_service = MockExecutionService::new();

//         mock_service
//             .expect_call()
//             .times(1)
//             .returning(move |_| Ok(ExecutionResponse::fake_builder().build().unwrap()));

//         let service_stack = ForbidMutations::new(PluginInit::new(
//             ForbidMutationsConfig(false),
//             Default::default(),
//         ))
//         .await
//         .expect("couldn't create forbid_mutations plugin")
//         .execution_service(mock_service.boxed());

//         let request = create_request(Method::GET, OperationKind::Mutation);

//         let _ = service_stack
//             .oneshot(request)
//             .await
//             .unwrap()
//             .next_response()
//             .await
//             .unwrap();
//     }

//     fn assert_error_matches(expected_error: &Error, response: Response) {
//         assert_eq!(&response.errors[0], expected_error);
//     }

//     fn create_request(method: Method, operation_kind: OperationKind) -> ExecutionRequest {
//         let root: PlanNode = if operation_kind == OperationKind::Mutation {
//             serde_json::from_value(json!({
//                 "kind": "Sequence",
//                 "nodes": [
//                     {
//                         "kind": "Fetch",
//                         "serviceName": "product",
//                         "variableUsages": [],
//                         "operation": "{__typename}",
//                         "operationKind": "mutation"
//                       },
//                 ]
//             }))
//             .unwrap()
//         } else {
//             serde_json::from_value(json!({
//                 "kind": "Sequence",
//                 "nodes": [
//                     {
//                         "kind": "Fetch",
//                         "serviceName": "product",
//                         "variableUsages": [],
//                         "operation": "{__typename}",
//                         "operationKind": "query"
//                       },
//                 ]
//             }))
//             .unwrap()
//         };

//         let request = Request::fake_builder()
//             .method(method)
//             .body(graphql::Request::default())
//             .build()
//             .expect("expecting valid request");
//         ExecutionRequest::fake_builder()
//             .supergraph_request(request)
//             .query_plan(QueryPlan::fake_builder().root(root).build())
//             .build()
//     }
// }

register_plugin!("apollo", "subscription", Subscription);