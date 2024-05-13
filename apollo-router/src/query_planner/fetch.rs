use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::ast;
use apollo_compiler::ast::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use indexmap::IndexSet;
use json_ext::PathElement;
use once_cell::sync::OnceCell as OnceLock;
use serde::Deserialize;
use serde::Serialize;
use tower::ServiceExt;
use tracing::instrument;
use tracing::Instrument;

use super::execution::ExecutionParameters;
use super::rewrites;
use super::rewrites::DataKeyRenamer;
use super::rewrites::DataRewrite;
use super::selection::execute_selection_set;
use super::selection::Selection;
use crate::error::Error;
use crate::error::FetchError;
use crate::error::ValidationErrors;
use crate::graphql;
use crate::graphql::Request;
use crate::http_ext;
use crate::json_ext;
use crate::json_ext::Object;
use crate::json_ext::Path;
use crate::json_ext::Value;
use crate::json_ext::ValueExt;
use crate::plugins::authorization::AuthorizationPlugin;
use crate::plugins::authorization::CacheKeyMetadata;
use crate::services::SubgraphRequest;
use crate::spec::query::change::QueryHashVisitor;
use crate::spec::Schema;

/// GraphQL operation type.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum OperationKind {
    #[default]
    Query,
    Mutation,
    Subscription,
}

impl Display for OperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.default_type_name())
    }
}

impl OperationKind {
    pub(crate) const fn default_type_name(&self) -> &'static str {
        match self {
            OperationKind::Query => "Query",
            OperationKind::Mutation => "Mutation",
            OperationKind::Subscription => "Subscription",
        }
    }

    /// Only for apollo studio exporter
    pub(crate) const fn as_apollo_operation_type(&self) -> &'static str {
        match self {
            OperationKind::Query => "query",
            OperationKind::Mutation => "mutation",
            OperationKind::Subscription => "subscription",
        }
    }
}

impl From<OperationKind> for ast::OperationType {
    fn from(value: OperationKind) -> Self {
        match value {
            OperationKind::Query => ast::OperationType::Query,
            OperationKind::Mutation => ast::OperationType::Mutation,
            OperationKind::Subscription => ast::OperationType::Subscription,
        }
    }
}

impl From<ast::OperationType> for OperationKind {
    fn from(value: ast::OperationType) -> Self {
        match value {
            ast::OperationType::Query => OperationKind::Query,
            ast::OperationType::Mutation => OperationKind::Mutation,
            ast::OperationType::Subscription => OperationKind::Subscription,
        }
    }
}

pub(crate) type SubgraphSchemas = HashMap<String, Arc<Valid<apollo_compiler::Schema>>>;

/// A fetch node.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FetchNode {
    /// The name of the service or subgraph that the fetch is querying.
    pub(crate) service_name: NodeStr,

    /// The data that is required for the subgraph fetch.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub(crate) requires: Vec<Selection>,

    /// The variables that are used for the subgraph fetch.
    pub(crate) variable_usages: Vec<NodeStr>,

    /// The GraphQL subquery that is used for the fetch.
    pub(crate) operation: SubgraphOperation,

    /// The GraphQL subquery operation name.
    pub(crate) operation_name: Option<NodeStr>,

    /// The GraphQL operation kind that is used for the fetch.
    pub(crate) operation_kind: OperationKind,

    /// Optional id used by Deferred nodes
    pub(crate) id: Option<NodeStr>,

    // Optionally describes a number of "rewrites" that query plan executors should apply to the data that is sent as input of this fetch.
    pub(crate) input_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that received from a fetch (and before it is applied to the current in-memory results).
    pub(crate) output_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // Optionally describes a number of "rewrites" to apply to the data that has already been received further up the tree
    pub(crate) context_rewrites: Option<Vec<rewrites::DataRewrite>>,

    // hash for the query and relevant parts of the schema. if two different schemas provide the exact same types, fields and directives
    // affecting the query, then they will have the same hash
    #[serde(default)]
    pub(crate) schema_aware_hash: Arc<QueryHash>,

    // authorization metadata for the subgraph query
    #[serde(default)]
    pub(crate) authorization: Arc<CacheKeyMetadata>,
}

#[derive(Clone)]
pub(crate) struct SubgraphOperation {
    // At least one of these two must be initialized
    serialized: OnceLock<String>,
    parsed: OnceLock<Arc<Valid<ExecutableDocument>>>,
}

impl SubgraphOperation {
    pub(crate) fn from_string(serialized: impl Into<String>) -> Self {
        Self {
            serialized: OnceLock::from(serialized.into()),
            parsed: OnceLock::new(),
        }
    }

    pub(crate) fn from_parsed(parsed: impl Into<Arc<Valid<ExecutableDocument>>>) -> Self {
        Self {
            serialized: OnceLock::new(),
            parsed: OnceLock::from(parsed.into()),
        }
    }

    pub(crate) fn as_serialized(&self) -> &str {
        self.serialized.get_or_init(|| {
            self.parsed
                .get()
                .expect("SubgraphOperation has neither representation initialized")
                .to_string()
        })
    }

    pub(crate) fn as_parsed(
        &self,
        subgraph_schema: &Valid<apollo_compiler::Schema>,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, ValidationErrors> {
        self.parsed.get_or_try_init(|| {
            let serialized = self
                .serialized
                .get()
                .expect("SubgraphOperation has neither representation initialized");
            Ok(Arc::new(
                ExecutableDocument::parse_and_validate(
                    subgraph_schema,
                    serialized,
                    "operation.graphql",
                )
                .map_err(|e| e.errors)?,
            ))
        })
    }
}

impl Serialize for SubgraphOperation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_serialized().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SubgraphOperation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self::from_string(String::deserialize(deserializer)?))
    }
}

impl PartialEq for SubgraphOperation {
    fn eq(&self, other: &Self) -> bool {
        self.as_serialized() == other.as_serialized()
    }
}

impl std::fmt::Debug for SubgraphOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_serialized(), f)
    }
}

impl std::fmt::Display for SubgraphOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.as_serialized(), f)
    }
}

#[derive(Clone, Default, Hash, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct QueryHash(#[serde(with = "hex")] pub(crate) Vec<u8>);

impl std::fmt::Debug for QueryHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("QueryHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl Display for QueryHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

pub(crate) struct Variables {
    pub(crate) variables: Object,
    pub(crate) inverted_paths: Vec<Vec<Path>>,
    pub(crate) contextual_args: Option<(HashSet<String>, usize)>,
}

fn query_batching_for_contextual_args(
    operation: &str,
    contextual_args: &Option<(HashSet<String>, usize)>,
) -> Option<String> {
    if let Some((ctx, times)) = contextual_args {
        let parser = apollo_compiler::Parser::new()
            .parse_ast(operation, "")
            // TODO: remove unwrap
            .unwrap();
        if let Some(mut operation) = parser
            .definitions
            .into_iter()
            .find_map(|definition| definition.as_operation_definition().cloned())
        {
            let mut new_variables: Vec<_> = Default::default();
            if operation
                .variables
                .iter()
                .any(|v| ctx.contains(v.name.as_str()))
            {
                let new_selection_set: Vec<_> = (0..*times)
                    .map(|i| {
                        // TODO: Unwrap
                        let mut s = operation.selection_set.first().unwrap().clone();
                        if let ast::Selection::Field(f) = &mut s {
                            let f = f.make_mut();
                            f.alias = Some(Name::new(format!("_{}", i)).unwrap());
                        }

                        for v in &operation.variables {
                            if ctx.contains(v.name.as_str()) {
                                let mut cloned = v.clone();
                                let new_variable = cloned.make_mut();
                                // TODO: remove unwrap
                                new_variable.name = Name::new(format!("{}_{}", v.name, i)).unwrap();
                                new_variables.push(Node::new(new_variable.clone()));

                                s = rename_variables(s, v.name.clone(), new_variable.name.clone());
                            } else if !new_variables.iter().any(|var| var.name == v.name) {
                                new_variables.push(v.clone());
                            }
                        }

                        s
                    })
                    .collect();

                let new_operation = operation.make_mut();
                new_operation.selection_set = new_selection_set;
                new_operation.variables = new_variables;

                return Some(new_operation.serialize().no_indent().to_string());
            }
        }
    }

    None
}

fn rename_variables(selection_set: ast::Selection, from: Name, to: Name) -> ast::Selection {
    match selection_set {
        ast::Selection::Field(f) => {
            let mut new = f.clone();

            let as_mut = new.make_mut();

            as_mut.arguments.iter_mut().for_each(|arg| {
                if arg.value.as_variable() == Some(&from) {
                    arg.make_mut().value = ast::Value::Variable(to.clone()).into();
                }
            });

            as_mut.selection_set = as_mut
                .selection_set
                .clone()
                .into_iter()
                .map(|s| rename_variables(s, from.clone(), to.clone()))
                .collect();

            ast::Selection::Field(new)
        }
        ast::Selection::InlineFragment(f) => {
            let mut new = f.clone();
            new.make_mut().selection_set = f
                .selection_set
                .clone()
                .into_iter()
                .map(|s| rename_variables(s, from.clone(), to.clone()))
                .collect();
            ast::Selection::InlineFragment(new)
        }
        ast::Selection::FragmentSpread(f) => ast::Selection::FragmentSpread(f),
    }
}

#[test]
fn test_query_batching_for_contextual_args() {
    let old_query = "query QueryLL__Subgraph1__1($representations:[_Any!]!$Subgraph1_U_field_a:String!){_entities(representations:$representations){...on U{id field(a:$Subgraph1_U_field_a)}}}";
    let mut ctx_args = HashSet::new();
    ctx_args.insert("Subgraph1_U_field_a".to_string());
    let contextual_args = Some((ctx_args, 2));

    let expected = "query QueryLL__Subgraph1__1($representations: [_Any!]!, $Subgraph1_U_field_a_0: String!, $Subgraph1_U_field_a_1: String!) { _0: _entities(representations: $representations) { ... on U { id field(a: $Subgraph1_U_field_a_0) } } _1: _entities(representations: $representations) { ... on U { id field(a: $Subgraph1_U_field_a_1) } } }";

    assert_eq!(
        expected,
        query_batching_for_contextual_args(old_query, &contextual_args).unwrap()
    );
}

// TODO: There is probably a function somewhere else that already does this
fn data_at_path<'v>(data: &'v Value, path: &Path) -> Option<&'v Value> {
    let v = match &path.0[0] {
        PathElement::Fragment(s) => {
            // get the value at data.get("__typename") and compare it with s. If the values are equal, return data, otherwise None
            let mut z: Option<&Value> = None;
            let wrapped_typename = data.get("__typename");
            if let Some(t) = wrapped_typename {
                if t.as_str() == Some(s.as_str()) {
                    z = Some(data);
                }
            }
            z
        }
        PathElement::Key(v, _) => data.get(v),
        PathElement::Index(idx) => Some(&data[idx]),
        PathElement::Flatten(_) => None,
    };

    if path.len() > 1 {
        if let Some(val) = v {
            let remaining_path = path.iter().skip(1).cloned().collect();
            return data_at_path(val, &remaining_path);
        }
    }
    v
}

fn merge_context_path(current_dir: &Path, context_path: &Path) -> Path {
    let mut i = 0;
    let mut j = current_dir.len();
    // iterate over the context_path(i), every time we encounter a '..', we want
    // to go up one level in the current_dir(j)
    while i < context_path.len() {
        match &context_path.0.get(i) {
            Some(PathElement::Key(e, _)) => {
                let mut found = false;
                if e == ".." {
                    while !found {
                        j -= 1;
                        if let Some(PathElement::Key(_, _)) = current_dir.0.get(j) {
                            found = true;
                        }
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    let mut return_path: Vec<PathElement> = current_dir.iter().take(j).cloned().collect();

    context_path.iter().skip(i).for_each(|e| {
        return_path.push(e.clone());
    });
    Path(return_path.into_iter().collect())
}

impl Variables {
    #[instrument(skip_all, level = "debug", name = "make_variables")]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        requires: &[Selection],
        variable_usages: &[NodeStr],
        data: &Value,
        current_dir: &Path,
        request: &Arc<http::Request<Request>>,
        schema: &Schema,
        input_rewrites: &Option<Vec<rewrites::DataRewrite>>,
        context_rewrites: &Option<Vec<rewrites::DataRewrite>>,
    ) -> Option<Variables> {
        let mut variables: serde_json_bytes::Map<serde_json_bytes::ByteString, Value> =
            Object::with_capacity(1 + variable_usages.len());

        if !requires.is_empty() {
            let mut inverted_paths: Vec<Vec<Path>> = Vec::new();
            let mut values: IndexSet<Value> = IndexSet::new();
            let mut named_args: Vec<HashMap<String, Value>> = Vec::new();
            data.select_values_and_paths(schema, current_dir, |path, value| {
                // first get contextual values that are required
                let mut found_rewrites: HashSet<String> = HashSet::new();
                let hash_map: HashMap<String, Value> = context_rewrites
                    .iter()
                    .flatten()
                    .filter_map(|rewrite| {
                        // note that it's possible that we could have multiple rewrites for the same variable. If that's the case, don't lookup if a value has already been found
                        match rewrite {
                            DataRewrite::KeyRenamer(item) => {
                                if !found_rewrites.contains(item.rename_key_to.as_str()) {
                                    let data_path = merge_context_path(path, &item.path);
                                    let val = data_at_path(data, &data_path);
                                    if let Some(v) = val {
                                        // add to found
                                        found_rewrites
                                            .insert(item.rename_key_to.clone().to_string());
                                        // TODO: not great
                                        let mut new_value = v.clone();
                                        if let Some(values) = new_value.as_array_mut() {
                                            for v in values {
                                                rewrites::apply_single_rewrite(
                                                    schema,
                                                    v,
                                                    &DataRewrite::KeyRenamer({
                                                        DataKeyRenamer {
                                                            path: data_path.clone(),
                                                            rename_key_to: item
                                                                .rename_key_to
                                                                .clone(),
                                                        }
                                                    }),
                                                );
                                            }
                                        } else {
                                            rewrites::apply_single_rewrite(
                                                schema,
                                                &mut new_value,
                                                &DataRewrite::KeyRenamer({
                                                    DataKeyRenamer {
                                                        path: data_path,
                                                        rename_key_to: item.rename_key_to.clone(),
                                                    }
                                                }),
                                            );
                                        }
                                        return Some((item.rename_key_to.to_string(), new_value));
                                    }
                                }
                                None
                            }
                            DataRewrite::ValueSetter(_) => {
                                // TODO: Log error? panic? not sure
                                None
                            }
                        }
                    })
                    .collect();

                let mut value = execute_selection_set(value, requires, schema, None);
                if value.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    rewrites::apply_rewrites(schema, &mut value, input_rewrites);
                    match values.get_index_of(&value) {
                        Some(index) => {
                            inverted_paths[index].push(path.clone());
                            named_args.push(hash_map);
                        }
                        None => {
                            inverted_paths.push(vec![path.clone()]);
                            values.insert(value);
                            debug_assert!(inverted_paths.len() == values.len());
                            named_args.push(hash_map);
                        }
                    }
                }
            });

            if values.is_empty() {
                return None;
            }

            let representations = Value::Array(Vec::from_iter(values));

            // Here we create a new map with all the key value pairs to push into variables.
            // Note that if all variables are the same, we just use the named parameter as a variable, but if they are different then each
            // entity will have it's own set of parameters all appended by _<index>
            let (extended_vars, contextual_args) = if let Some(first_map) = named_args.first() {
                if named_args.iter().all(|map| map == first_map) {
                    (
                        first_map
                            .iter()
                            .map(|(k, v)| (k.as_str().into(), v.clone()))
                            .collect(),
                        None,
                    )
                } else {
                    let mut hash_map: HashMap<String, Value> = HashMap::new();
                    let arg_names: HashSet<_> = first_map.keys().cloned().collect();
                    for (index, item) in named_args.iter().enumerate() {
                        // append _<index> to each of the arguments and push all the values into hash_map
                        hash_map.extend(item.iter().map(|(k, v)| {
                            let mut new_named_param = k.clone();
                            new_named_param.push_str(&format!("_{}", index));
                            (new_named_param, v.clone())
                        }));
                    }
                    (hash_map, Some((arg_names, named_args.len())))
                }
            } else {
                (HashMap::new(), None)
            };

            let body = request.body();
            variables.extend(
                extended_vars
                    .iter()
                    .map(|(key, value)| (key.as_str().into(), value.clone())),
            );
            variables.extend(variable_usages.iter().filter_map(|key| {
                body.variables
                    .get_key_value(key.as_str())
                    .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
            }));

            variables.insert("representations", representations);
            Some(Variables {
                variables,
                inverted_paths,
                contextual_args,
            })
        } else {
            // with nested operations (Query or Mutation has an operation returning a Query or Mutation),
            // when the first fetch fails, the query plan will still execute up until the second fetch,
            // where `requires` is empty (not a federated fetch), the current dir is not emmpty (child of
            // the previous operation field) and the data is null. In that case, we recognize that we
            // should not perform the next fetch
            if !current_dir.is_empty()
                && data
                    .get_path(schema, current_dir)
                    .map(|value| value.is_null())
                    .unwrap_or(true)
            {
                return None;
            }

            Some(Variables {
                variables: variable_usages
                    .iter()
                    .filter_map(|key| {
                        variables
                            .get_key_value(key.as_str())
                            .map(|(variable_key, value)| (variable_key.clone(), value.clone()))
                    })
                    .collect::<Object>(),
                inverted_paths: Vec::new(),
                contextual_args: None,
            })
        }
    }
}

impl FetchNode {
    pub(crate) fn parsed_operation(
        &self,
        subgraph_schemas: &SubgraphSchemas,
    ) -> Result<&Arc<Valid<ExecutableDocument>>, ValidationErrors> {
        self.operation
            .as_parsed(&subgraph_schemas[self.service_name.as_str()])
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn fetch_node<'a>(
        &'a self,
        parameters: &'a ExecutionParameters<'a>,
        data: &'a Value,
        current_dir: &'a Path,
    ) -> (Value, Vec<Error>) {
        let FetchNode {
            operation,
            operation_kind,
            operation_name,
            service_name,
            ..
        } = self;

        let Variables {
            variables,
            inverted_paths: paths,
            contextual_args,
        } = match Variables::new(
            &self.requires,
            &self.variable_usages,
            data,
            current_dir,
            // Needs the original request here
            parameters.supergraph_request,
            parameters.schema,
            &self.input_rewrites,
            &self.context_rewrites,
        ) {
            Some(variables) => variables,
            None => {
                return (Value::Object(Object::default()), Vec::new());
            }
        };

        let query_batched_query =
            query_batching_for_contextual_args(operation.as_serialized(), &contextual_args);

        let mut subgraph_request = SubgraphRequest::builder()
            .supergraph_request(parameters.supergraph_request.clone())
            .subgraph_request(
                http_ext::Request::builder()
                    .method(http::Method::POST)
                    .uri(
                        parameters
                            .schema
                            .subgraph_url(service_name)
                            .unwrap_or_else(|| {
                                panic!(
                                    "schema uri for subgraph '{service_name}' should already have been checked"
                                )
                            })
                            .clone(),
                    )
                    .body(
                        Request::builder()
                            .query(query_batched_query.as_deref().unwrap_or(operation.as_serialized()))
                            .and_operation_name(operation_name.as_ref().map(|n| n.to_string()))
                            .variables(variables.clone())
                            .build(),
                    )
                    .build()
                    .expect("it won't fail because the url is correct and already checked; qed"),
            )
            .subgraph_name(self.service_name.to_string())
            .operation_kind(*operation_kind)
            .context(parameters.context.clone())
            .build();
        subgraph_request.query_hash = self.schema_aware_hash.clone();
        subgraph_request.authorization = self.authorization.clone();

        let service = parameters
            .service_factory
            .create(service_name)
            .expect("we already checked that the service exists during planning; qed");

        let (_parts, response) = match service
            .oneshot(subgraph_request)
            .instrument(tracing::trace_span!("subfetch_stream"))
            .await
            // TODO this is a problem since it restores details about failed service
            // when errors have been redacted in the include_subgraph_errors module.
            // Unfortunately, not easy to fix here, because at this point we don't
            // know if we should be redacting errors for this subgraph...
            .map_err(|e| match e.downcast::<FetchError>() {
                Ok(inner) => match *inner {
                    FetchError::SubrequestHttpError { .. } => *inner,
                    _ => FetchError::SubrequestHttpError {
                        status_code: None,
                        service: service_name.to_string(),
                        reason: inner.to_string(),
                    },
                },
                Err(e) => FetchError::SubrequestHttpError {
                    status_code: None,
                    service: service_name.to_string(),
                    reason: e.to_string(),
                },
            }) {
            Err(e) => {
                return (
                    Value::default(),
                    vec![e.to_graphql_error(Some(current_dir.to_owned()))],
                );
            }
            Ok(res) => res.response.into_parts(),
        };

        super::log::trace_subfetch(
            service_name,
            operation.as_serialized(),
            &variables,
            &response,
        );

        if !response.is_primary() {
            return (
                Value::default(),
                vec![FetchError::SubrequestUnexpectedPatchResponse {
                    service: service_name.to_string(),
                }
                .to_graphql_error(Some(current_dir.to_owned()))],
            );
        }
        let (value, errors) =
            self.response_at_path(parameters.schema, current_dir, paths, response);
        if let Some(id) = &self.id {
            if let Some(sender) = parameters.deferred_fetches.get(id.as_str()) {
                tracing::info!(monotonic_counter.apollo.router.operations.defer.fetch = 1u64);
                if let Err(e) = sender.clone().send((value.clone(), errors.clone())) {
                    tracing::error!("error sending fetch result at path {} and id {:?} for deferred response building: {}", current_dir, self.id, e);
                }
            }
        }
        (value, errors)
    }

    #[instrument(skip_all, level = "debug", name = "response_insert")]
    fn response_at_path<'a>(
        &'a self,
        schema: &Schema,
        current_dir: &'a Path,
        inverted_paths: Vec<Vec<Path>>,
        response: graphql::Response,
    ) -> (Value, Vec<Error>) {
        if !self.requires.is_empty() {
            let entities_path = Path(vec![json_ext::PathElement::Key(
                "_entities".to_string(),
                None,
            )]);

            let mut errors: Vec<Error> = vec![];
            for mut error in response.errors {
                // the locations correspond to the subgraph query and cannot be linked to locations
                // in the client query, so we remove them
                error.locations = Vec::new();

                // errors with path should be updated to the path of the entity they target
                if let Some(ref path) = error.path {
                    if path.starts_with(&entities_path) {
                        // the error's path has the format '/_entities/1/other' so we ignore the
                        // first element and then get the index
                        match path.0.get(1) {
                            Some(json_ext::PathElement::Index(i)) => {
                                for values_path in
                                    inverted_paths.get(*i).iter().flat_map(|v| v.iter())
                                {
                                    errors.push(Error {
                                        locations: error.locations.clone(),
                                        // append to the entitiy's path the error's path without
                                        //`_entities` and the index
                                        path: Some(Path::from_iter(
                                            values_path.0.iter().chain(&path.0[2..]).cloned(),
                                        )),
                                        message: error.message.clone(),
                                        extensions: error.extensions.clone(),
                                    })
                                }
                            }
                            _ => {
                                error.path = Some(current_dir.clone());
                                errors.push(error)
                            }
                        }
                    } else {
                        error.path = Some(current_dir.clone());
                        errors.push(error);
                    }
                } else {
                    errors.push(error);
                }
            }

            // we have to nest conditions and do early returns here
            // because we need to take ownership of the inner value
            if let Some(Value::Object(mut map)) = response.data {
                if let Some(entities) = map.remove("_entities") {
                    tracing::trace!("received entities: {:?}", &entities);

                    if let Value::Array(array) = entities {
                        let mut value = Value::default();

                        for (index, mut entity) in array.into_iter().enumerate() {
                            rewrites::apply_rewrites(schema, &mut entity, &self.output_rewrites);

                            if let Some(paths) = inverted_paths.get(index) {
                                if paths.len() > 1 {
                                    for path in &paths[1..] {
                                        let _ = value.insert(path, entity.clone());
                                    }
                                }

                                if let Some(path) = paths.first() {
                                    let _ = value.insert(path, entity);
                                }
                            }
                        }
                        return (value, errors);
                    }
                }
            }

            // if we get here, it means that the response was missing the `_entities` key
            // This can happen if the subgraph failed during query execution e.g. for permissions checks.
            // In this case we should add an additional error because the subgraph should have returned an error that will be bubbled up to the client.
            // However, if they have not then print a warning to the logs.
            if errors.is_empty() {
                tracing::warn!(
                    "Subgraph response from '{}' was missing key `_entities` and had no errors. This is likely a bug in the subgraph.",
                    self.service_name
                );
            }

            (Value::Null, errors)
        } else {
            let current_slice =
                if matches!(current_dir.last(), Some(&json_ext::PathElement::Flatten(_))) {
                    &current_dir.0[..current_dir.0.len() - 1]
                } else {
                    &current_dir.0[..]
                };

            let errors: Vec<Error> = response
                .errors
                .into_iter()
                .map(|error| {
                    let path = error.path.as_ref().map(|path| {
                        Path::from_iter(current_slice.iter().chain(path.iter()).cloned())
                    });

                    Error {
                        locations: error.locations,
                        path,
                        message: error.message,
                        extensions: error.extensions,
                    }
                })
                .collect();
            let mut data = response.data.unwrap_or_default();
            rewrites::apply_rewrites(schema, &mut data, &self.output_rewrites);
            (Value::from_path(current_dir, data), errors)
        }
    }

    #[cfg(test)]
    pub(crate) fn service_name(&self) -> &str {
        &self.service_name
    }

    pub(crate) fn operation_kind(&self) -> &OperationKind {
        &self.operation_kind
    }

    pub(crate) fn hash_subquery(
        &mut self,
        subgraph_schemas: &SubgraphSchemas,
        supergraph_schema_hash: &str,
    ) -> Result<(), ValidationErrors> {
        let doc = self.parsed_operation(subgraph_schemas)?;
        let schema = &subgraph_schemas[self.service_name.as_str()];

        if let Ok(hash) = QueryHashVisitor::hash_query(
            schema,
            supergraph_schema_hash,
            doc,
            self.operation_name.as_deref(),
        ) {
            self.schema_aware_hash = Arc::new(QueryHash(hash));
        }
        Ok(())
    }

    pub(crate) fn extract_authorization_metadata(
        &mut self,
        schema: &Valid<apollo_compiler::Schema>,
        global_authorisation_cache_key: &CacheKeyMetadata,
    ) {
        let doc = ExecutableDocument::parse(
            schema,
            self.operation.as_serialized().to_string(),
            "query.graphql",
        )
        // Assume query planing creates a valid document: ignore parse errors
        .unwrap_or_else(|invalid| invalid.partial);
        let subgraph_query_cache_key = AuthorizationPlugin::generate_cache_metadata(
            &doc,
            self.operation_name.as_deref(),
            schema,
            !self.requires.is_empty(),
        );

        // we need to intersect the cache keys because the global key already takes into account
        // the scopes and policies from the client request
        self.authorization = Arc::new(AuthorizationPlugin::intersect_cache_keys_subgraph(
            global_authorisation_cache_key,
            &subgraph_query_cache_key,
        ));
    }
}
