use apollo_compiler::name;
use apollo_compiler::ExecutableDocument;
use indexmap::IndexSet;

use super::normalize_operation;
use super::Containment;
use super::ContainmentOptions;
use super::Operation;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;
use crate::subgraph::Subgraph;

fn parse_schema_and_operation(
    schema_and_operation: &str,
) -> (ValidFederationSchema, ExecutableDocument) {
    let (schema, executable_document) =
        apollo_compiler::parse_mixed_validate(schema_and_operation, "document.graphql").unwrap();
    let executable_document = executable_document.into_inner();
    let schema = ValidFederationSchema::new(schema).unwrap();
    (schema, executable_document)
}

fn parse_subgraph(name: &str, schema: &str) -> ValidFederationSchema {
    let parsed_schema =
        Subgraph::parse_and_expand(name, &format!("https://{name}"), schema).unwrap();
    ValidFederationSchema::new(parsed_schema.schema).unwrap()
}

#[test]
fn expands_named_fragments() {
    let operation_with_named_fragment = r#"
query NamedFragmentQuery {
  foo {
    id
    ...Bar
  }
}

fragment Bar on Foo {
  bar
  baz
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: Int
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_named_fragment);
    if let Some(operation) = executable_document
        .named_operations
        .get_mut("NamedFragmentQuery")
    {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();

        let expected = r#"query NamedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

#[test]
fn expands_and_deduplicates_fragments() {
    let operation_with_named_fragment = r#"
query NestedFragmentQuery {
  foo {
    ...FirstFragment
    ...SecondFragment
  }
}

fragment FirstFragment on Foo {
  id
  bar
  baz
}

fragment SecondFragment on Foo {
  id
  bar
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_named_fragment);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();

        let expected = r#"query NestedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

#[test]
fn can_remove_introspection_selections() {
    let operation_with_introspection = r#"
query TestIntrospectionQuery {
  __schema {
    types {
      name
    }
  }
}

type Query {
  foo: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_introspection);
    if let Some(operation) = executable_document
        .named_operations
        .get_mut("TestIntrospectionQuery")
    {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();

        assert!(normalized_operation.selection_set.selections.is_empty());
    }
}

#[test]
fn merge_same_fields_without_directives() {
    let operation_string = r#"
query Test {
  t {
    v1
  }
  t {
    v2
 }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_string);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fields_with_same_directive() {
    let operation_with_directives = r#"
query Test($skipIf: Boolean!) {
  t @skip(if: $skipIf) {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_with_directives);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t @skip(if: $skipIf) {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fields_with_same_directive_but_different_arg_order() {
    let operation_with_directives_different_arg_order = r#"
query Test($skipIf: Boolean!) {
  t @customSkip(if: $skipIf, label: "foo") {
    v1
  }
  t @customSkip(label: "foo", if: $skipIf) {
    v2
  }
}

directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_directives_different_arg_order);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t @customSkip(if: $skipIf, label: "foo") {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_only_one_field_specifies_directive() {
    let operation_one_field_with_directives = r#"
query Test($skipIf: Boolean!) {
  t {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_one_field_with_directives);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    v1
  }
  t @skip(if: $skipIf) {
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_fields_have_different_directives() {
    let operation_different_directives = r#"
query Test($skip1: Boolean!, $skip2: Boolean!) {
  t @skip(if: $skip1) {
    v1
  }
  t @skip(if: $skip2) {
    v2
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_different_directives);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skip1: Boolean!, $skip2: Boolean!) {
  t @skip(if: $skip1) {
    v1
  }
  t @skip(if: $skip2) {
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_fields_with_defer_directive() {
    let operation_defer_fields = r#"
query Test {
  t {
    ... @defer {
      v1
    }
  }
  t {
    ... @defer {
      v2
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_defer_fields);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    ... @defer {
      v1
    }
    ... @defer {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_nested_field_selections() {
    let nested_operation = r#"
query Test {
  t {
    t1
    ... @defer {
      v {
        v1
      }
    }
  }
  t {
    t1
    t2
    ... @defer {
      v {
        v2
      }
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  t1: Int
  t2: String
  v: V
}

type V {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(nested_operation);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    t1
    ... @defer {
      v {
        v1
      }
    }
    t2
    ... @defer {
      v {
        v2
      }
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

//
// inline fragments
//

#[test]
fn merge_same_fragment_without_directives() {
    let operation_with_fragments = r#"
query Test {
  t {
    ... on T {
      v1
    }
    ... on T {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_with_fragments);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fragments_with_same_directives() {
    let operation_fragments_with_directives = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T @skip(if: $skipIf) {
      v1
    }
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_directives);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    ... on T @skip(if: $skipIf) {
      v1
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_same_fragments_with_same_directive_but_different_arg_order() {
    let operation_fragments_with_directives_args_order = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T @customSkip(if: $skipIf, label: "foo") {
      v1
    }
    ... on T @customSkip(label: "foo", if: $skipIf) {
      v2
    }
  }
}

directive @customSkip(if: Boolean!, label: String!) on FIELD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_directives_args_order);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    ... on T @customSkip(if: $skipIf, label: "foo") {
      v1
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_only_one_fragment_specifies_directive() {
    let operation_one_fragment_with_directive = r#"
query Test($skipIf: Boolean!) {
  t {
    ... on T {
      v1
    }
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_one_fragment_with_directive);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skipIf: Boolean!) {
  t {
    v1
    ... on T @skip(if: $skipIf) {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_when_fragments_have_different_directives() {
    let operation_fragments_with_different_directive = r#"
query Test($skip1: Boolean!, $skip2: Boolean!) {
  t {
    ... on T @skip(if: $skip1) {
      v1
    }
    ... on T @skip(if: $skip2) {
      v2
    }
  }
}

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_different_directive);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test($skip1: Boolean!, $skip2: Boolean!) {
  t {
    ... on T @skip(if: $skip1) {
      v1
    }
    ... on T @skip(if: $skip2) {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn do_not_merge_fragments_with_defer_directive() {
    let operation_fragments_with_defer = r#"
query Test {
  t {
    ... on T @defer {
      v1
    }
    ... on T @defer {
      v2
    }
  }
}

directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

type Query {
  t: T
}

type T {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_fragments_with_defer);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    ... on T @defer {
      v1
    }
    ... on T @defer {
      v2
    }
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn merge_nested_fragments() {
    let operation_nested_fragments = r#"
query Test {
  t {
    ... on T {
      t1
    }
    ... on T {
      v {
        v1
      }
    }
  }
  t {
    ... on T {
      t1
      t2
    }
    ... on T {
      v {
        v2
      }
    }
  }
}

type Query {
  t: T
}

type T {
  t1: Int
  t2: String
  v: V
}

type V {
  v1: Int
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_nested_fragments);
    if let Some((_, operation)) = executable_document.named_operations.first_mut() {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query Test {
  t {
    t1
    v {
      v1
      v2
    }
    t2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    } else {
        panic!("unable to parse document")
    }
}

#[test]
fn removes_sibling_typename() {
    let operation_with_typename = r#"
query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}

type Query {
  foo: Foo
}

type Foo {
  v1: ID!
  v2: String
}
"#;
    let (schema, mut executable_document) = parse_schema_and_operation(operation_with_typename);
    if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query TestQuery {
  foo {
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

#[test]
fn keeps_typename_if_no_other_selection() {
    let operation_with_single_typename = r#"
query TestQuery {
  foo {
    __typename
  }
}

type Query {
  foo: Foo
}

type Foo {
  v1: ID!
  v2: String
}
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_single_typename);
    if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &IndexSet::new(),
        )
        .unwrap();
        let expected = r#"query TestQuery {
  foo {
    __typename
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

#[test]
fn keeps_typename_for_interface_object() {
    let operation_with_intf_object_typename = r#"
query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}

directive @interfaceObject on OBJECT
directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

type Query {
  foo: Foo
}

type Foo @interfaceObject @key(fields: "id") {
  v1: ID!
  v2: String
}

scalar FieldSet
"#;
    let (schema, mut executable_document) =
        parse_schema_and_operation(operation_with_intf_object_typename);
    if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
        let mut interface_objects: IndexSet<InterfaceTypeDefinitionPosition> = IndexSet::new();
        interface_objects.insert(InterfaceTypeDefinitionPosition {
            type_name: name!("Foo"),
        });

        let normalized_operation = normalize_operation(
            operation,
            &executable_document.fragments,
            &schema,
            &interface_objects,
        )
        .unwrap();
        let expected = r#"query TestQuery {
  foo {
    __typename
    v1
    v2
  }
}"#;
        let actual = normalized_operation.to_string();
        assert_eq!(expected, actual);
    }
}

//
// REBASE TESTS
//
#[cfg(test)]
mod rebase_tests {
    use apollo_compiler::name;
    use indexmap::IndexSet;

    use crate::query_plan::operation::normalize_operation;
    use crate::query_plan::operation::tests::parse_schema_and_operation;
    use crate::query_plan::operation::tests::parse_subgraph;
    use crate::schema::position::InterfaceTypeDefinitionPosition;

    #[test]
    fn skips_unknown_fragment_fields() {
        let operation_fragments = r#"
query TestQuery {
  t {
    ...FragOnT
  }
}

fragment FragOnT on T {
  v0
  v1
  v2
  u1 {
    v3
    v4
    v5
  }
  u2 {
    v4
    v5
  }
}

type Query {
  t: T
}

type T {
  v0: Int
  v1: Int
  v2: Int
  u1: U
  u2: U
}

type U {
  v3: Int
  v4: Int
  v5: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  _: Int
}

type T {
  v1: Int
  u1: U
}

type U {
  v3: Int
  v5: Int
}"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnT")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnT").unwrap();

            insta::assert_snapshot!(rebased_fragment, @r###"
                    fragment FragOnT on T {
                      v1
                      u1 {
                        v3
                        v5
                      }
                    }
                "###);
        }
    }

    #[test]
    fn skips_unknown_fragment_on_condition() {
        let operation_fragments = r#"
query TestQuery {
  t {
    ...FragOnT
  }
  u {
    ...FragOnU
  }
}

fragment FragOnT on T {
  x
  y
}

fragment FragOnU on U {
  x
  y
}

type Query {
  t: T
  u: U
}

type T {
  x: Int
  y: Int
}

type U {
  x: Int
  y: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );
        assert_eq!(2, executable_document.fragments.len());

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
  y: Int
}"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnT")));
            assert!(!rebased_fragments.contains(&name!("FragOnU")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnT").unwrap();

            let expected = r#"fragment FragOnT on T {
  x
  y
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn skips_unknown_type_within_fragment() {
        let operation_fragments = r#"
query TestQuery {
  i {
    ...FragOnI
  }
}

fragment FragOnI on I {
  id
  otherId
  ... on T1 {
    x
  }
  ... on T2 {
    y
  }
}

type Query {
  i: I
}

interface I {
  id: ID!
  otherId: ID!
}

type T1 implements I {
  id: ID!
  otherId: ID!
  x: Int
}

type T2 implements I {
  id: ID!
  otherId: ID!
  y: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  i: I
}

interface I {
  id: ID!
}

type T2 implements I {
  id: ID!
  y: Int
}
"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnI")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnI").unwrap();

            let expected = r#"fragment FragOnI on I {
  id
  ... on T2 {
    y
  }
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn skips_typename_on_possible_interface_objects_within_fragment() {
        let operation_fragments = r#"
query TestQuery {
  i {
    ...FragOnI
  }
}

fragment FragOnI on I {
  __typename
  id
  x
}

type Query {
  i: I
}

interface I {
  id: ID!
  x: String!
}

type T implements I {
  id: ID!
  x: String!
}
"#;

        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let mut interface_objects: IndexSet<InterfaceTypeDefinitionPosition> = IndexSet::new();
            interface_objects.insert(InterfaceTypeDefinitionPosition {
                type_name: name!("I"),
            });
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &interface_objects,
            )
            .unwrap();

            let subgraph_schema = r#"extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.5", import: [{ name: "@interfaceObject" }, { name: "@key" }])

directive @link(url: String, as: String, import: [link__Import]) repeatable on SCHEMA

directive @key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

directive @interfaceObject on OBJECT

type Query {
  i: I
}

type I @interfaceObject @key(fields: "id") {
  id: ID!
  x: String!
}

scalar link__Import

scalar federation__FieldSet
"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            assert!(!rebased_fragments.is_empty());
            assert!(rebased_fragments.contains(&name!("FragOnI")));
            let rebased_fragment = rebased_fragments.fragments.get("FragOnI").unwrap();

            let expected = r#"fragment FragOnI on I {
  id
  x
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn skips_fragments_with_trivial_selections() {
        let operation_fragments = r#"
query TestQuery {
  t {
    ...F1
    ...F2
    ...F3
  }
}

fragment F1 on T {
  a
  b
}

fragment F2 on T {
  __typename
  a
  b
}

fragment F3 on T {
  __typename
  a
  b
  c
  d
}

type Query {
  t: T
}

type T {
  a: Int
  b: Int
  c: Int
  d: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  c: Int
  d: Int
}
"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
            assert_eq!(1, rebased_fragments.size());
            assert!(rebased_fragments.contains(&name!("F3")));
            let rebased_fragment = rebased_fragments.fragments.get("F3").unwrap();

            let expected = r#"fragment F3 on T {
  __typename
  c
  d
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn handles_skipped_fragments_within_fragments() {
        let operation_fragments = r#"
query TestQuery {
  ...TheQuery
}

fragment TheQuery on Query {
  t {
    x
    ... GetU
  }
}

fragment GetU on T {
  u {
    y
    z
  }
}

type Query {
  t: T
}

type T {
  x: Int
  u: U
}

type U {
  y: Int
  z: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
}"#;
            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
            assert_eq!(1, rebased_fragments.size());
            assert!(rebased_fragments.contains(&name!("TheQuery")));
            let rebased_fragment = rebased_fragments.fragments.get("TheQuery").unwrap();

            let expected = r#"fragment TheQuery on Query {
  t {
    x
  }
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn handles_subtypes_within_subgraphs() {
        let operation_fragments = r#"
query TestQuery {
  ...TQuery
}

fragment TQuery on Query {
  t {
    x
    y
    ... on T {
      z
    }
  }
}

type Query {
  t: I
}

interface I {
  x: Int
  y: Int
}

type T implements I {
  x: Int
  y: Int
  z: Int
}
"#;
        let (schema, mut executable_document) = parse_schema_and_operation(operation_fragments);
        assert!(
            !executable_document.fragments.is_empty(),
            "operation should have some fragments"
        );

        if let Some(operation) = executable_document.named_operations.get_mut("TestQuery") {
            let normalized_operation = normalize_operation(
                operation,
                &executable_document.fragments,
                &schema,
                &IndexSet::new(),
            )
            .unwrap();

            let subgraph_schema = r#"type Query {
  t: T
}

type T {
  x: Int
  y: Int
  z: Int
}
"#;

            let subgraph = parse_subgraph("A", subgraph_schema);
            let rebased_fragments = normalized_operation.named_fragments.rebase_on(&subgraph);
            assert!(rebased_fragments.is_ok());
            let rebased_fragments = rebased_fragments.unwrap();
            // F1 reduces to nothing, and F2 reduces to just __typename so we shouldn't keep them.
            assert_eq!(1, rebased_fragments.size());
            assert!(rebased_fragments.contains(&name!("TQuery")));
            let rebased_fragment = rebased_fragments.fragments.get("TQuery").unwrap();

            let expected = r#"fragment TQuery on Query {
  t {
    x
    y
    z
  }
}"#;
            let actual = rebased_fragment.to_string();
            assert_eq!(actual, expected);
        }
    }
}

fn containment_custom(left: &str, right: &str, ignore_missing_typename: bool) -> Containment {
    let schema = apollo_compiler::Schema::parse_and_validate(
        r#"
        directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

        interface Intf {
            intfField: Int
        }
        type HasA implements Intf {
            a: Boolean
            intfField: Int
        }
        type Nested {
            a: Int
            b: Int
            c: Int
        }
        input Input {
            recur: Input
            f: Boolean
            g: Boolean
            h: Boolean
        }
        type Query {
            a: Int
            b: Int
            c: Int
            object: Nested
            intf: Intf
            arg(a: Int, b: Int, c: Int, d: Input): Int
        }
        "#,
        "schema.graphql",
    )
    .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    let left = Operation::parse(schema.clone(), left, "left.graphql", None).unwrap();
    let right = Operation::parse(schema.clone(), right, "right.graphql", None).unwrap();

    left.selection_set.containment(
        &right.selection_set,
        ContainmentOptions {
            ignore_missing_typename,
        },
    )
}

fn containment(left: &str, right: &str) -> Containment {
    containment_custom(left, right, false)
}

#[test]
fn selection_set_contains() {
    assert_eq!(containment("{ a }", "{ a }"), Containment::Equal);
    assert_eq!(containment("{ a b }", "{ b a }"), Containment::Equal);
    assert_eq!(
        containment("{ arg(a: 1) }", "{ arg(a: 2) }"),
        Containment::NotContained
    );
    assert_eq!(
        containment("{ arg(a: 1) }", "{ arg(b: 1) }"),
        Containment::NotContained
    );
    assert_eq!(
        containment("{ arg(a: 1) }", "{ arg(a: 1) }"),
        Containment::Equal
    );
    assert_eq!(
        containment("{ arg(a: 1, b: 1) }", "{ arg(b: 1 a: 1) }"),
        Containment::Equal
    );
    assert_eq!(
        containment("{ arg(a: 1) }", "{ arg(a: 1) }"),
        Containment::Equal
    );
    assert_eq!(
        containment(
            "{ arg(d: { f: true, g: true }) }",
            "{ arg(d: { f: true }) }"
        ),
        Containment::NotContained
    );
    assert_eq!(
        containment(
            "{ arg(d: { recur: { f: true } g: true h: false }) }",
            "{ arg(d: { h: false recur: {f: true} g: true }) }"
        ),
        Containment::Equal
    );
    assert_eq!(
        containment("{ arg @skip(if: true) }", "{ arg @skip(if: true) }"),
        Containment::Equal
    );
    assert_eq!(
        containment("{ arg @skip(if: true) }", "{ arg @skip(if: false) }"),
        Containment::NotContained
    );
    assert_eq!(
        containment("{ ... @defer { arg } }", "{ ... @defer { arg } }"),
        Containment::NotContained,
        "@defer selections never contain each other"
    );
    assert_eq!(
        containment("{ a b c }", "{ b a }"),
        Containment::StrictlyContained
    );
    assert_eq!(
        containment("{ a b }", "{ b c a }"),
        Containment::NotContained
    );
    assert_eq!(containment("{ a }", "{ b }"), Containment::NotContained);
    assert_eq!(
        containment("{ object { a } }", "{ object { b a } }"),
        Containment::NotContained
    );

    assert_eq!(
        containment("{ ... { a } }", "{ ... { a } }"),
        Containment::Equal
    );
    assert_eq!(
        containment(
            "{ intf { ... on HasA { a } } }",
            "{ intf { ... on HasA { a } } }",
        ),
        Containment::Equal
    );
    // These select the same things, but containment also counts fragment namedness
    assert_eq!(
        containment(
            "{ intf { ... on HasA { a } } }",
            "{ intf { ...named } } fragment named on HasA { a }",
        ),
        Containment::NotContained
    );
    assert_eq!(
        containment(
            "{ intf { ...named } } fragment named on HasA { a intfField }",
            "{ intf { ...named } } fragment named on HasA { a }",
        ),
        Containment::StrictlyContained
    );
    assert_eq!(
        containment(
            "{ intf { ...named } } fragment named on HasA { a }",
            "{ intf { ...named } } fragment named on HasA { a intfField }",
        ),
        Containment::NotContained
    );
}

#[test]
fn selection_set_contains_missing_typename() {
    assert_eq!(
        containment_custom("{ a }", "{ a __typename }", true),
        Containment::Equal
    );
    assert_eq!(
        containment_custom("{ a b }", "{ b a __typename }", true),
        Containment::Equal
    );
    assert_eq!(
        containment_custom("{ a b }", "{ b __typename }", true),
        Containment::StrictlyContained
    );
    assert_eq!(
        containment_custom("{ object { a b } }", "{ object { b __typename } }", true),
        Containment::StrictlyContained
    );
    assert_eq!(
        containment_custom(
            "{ intf { intfField __typename } }",
            "{ intf { intfField } }",
            true
        ),
        Containment::StrictlyContained,
    );
    assert_eq!(
        containment_custom(
            "{ intf { intfField __typename } }",
            "{ intf { intfField __typename } }",
            true
        ),
        Containment::Equal,
    );
}

/// This regression-tests an assumption from
/// https://github.com/apollographql/federation-next/pull/290#discussion_r1587200664
#[test]
fn converting_operation_types() {
    let schema = apollo_compiler::Schema::parse_and_validate(
        r#"
        interface Intf {
            intfField: Int
        }
        type HasA implements Intf {
            a: Boolean
            intfField: Int
        }
        type Nested {
            a: Int
            b: Int
            c: Int
        }
        type Query {
            a: Int
            b: Int
            c: Int
            object: Nested
            intf: Intf
        }
        "#,
        "schema.graphql",
    )
    .unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    insta::assert_snapshot!(Operation::parse(
            schema.clone(),
            r#"
        {
            intf {
                ... on HasA { a }
                ... frag
            }
        }
        fragment frag on HasA { intfField }
        "#,
            "operation.graphql",
            None,
        )
        .unwrap(), @r###"
        {
          intf {
            ... on HasA {
              a
            }
            ...frag
          }
        }
        "###);
}
