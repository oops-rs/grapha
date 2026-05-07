use grapha::fields::FieldSet;

#[test]
fn default_has_file_true_rest_false() {
    let fs = FieldSet::default();
    assert!(!fs.score);
    assert!(fs.file);
    assert!(!fs.id);
    assert!(!fs.module);
    assert!(!fs.span);
    assert!(!fs.snippet);
    assert!(!fs.visibility);
    assert!(!fs.signature);
    assert!(!fs.doc_comment);
    assert!(!fs.annotation);
    assert!(!fs.role);
}

#[test]
fn parse_all_enables_every_field() {
    let fs = FieldSet::parse("all");
    assert!(fs.score);
    assert!(fs.file);
    assert!(fs.id);
    assert!(fs.module);
    assert!(fs.span);
    assert!(fs.snippet);
    assert!(fs.visibility);
    assert!(fs.signature);
    assert!(fs.doc_comment);
    assert!(fs.annotation);
    assert!(fs.role);
}

#[test]
fn parse_full_alias_enables_every_field() {
    let fs = FieldSet::parse("full");
    assert!(fs.score);
    assert!(fs.file);
    assert!(fs.id);
    assert!(fs.module);
    assert!(fs.span);
    assert!(fs.snippet);
    assert!(fs.visibility);
    assert!(fs.signature);
    assert!(fs.doc_comment);
    assert!(fs.annotation);
    assert!(fs.role);
}

#[test]
fn parse_none_disables_every_field() {
    let fs = FieldSet::parse("none");
    assert!(!fs.score);
    assert!(!fs.file);
    assert!(!fs.id);
    assert!(!fs.module);
    assert!(!fs.span);
    assert!(!fs.snippet);
    assert!(!fs.visibility);
    assert!(!fs.signature);
    assert!(!fs.doc_comment);
    assert!(!fs.annotation);
    assert!(!fs.role);
}

#[test]
fn parse_comma_separated_fields() {
    let fs = FieldSet::parse("file,id,span");
    assert!(fs.file);
    assert!(fs.id);
    assert!(!fs.module);
    assert!(fs.span);
    assert!(!fs.snippet);
    assert!(!fs.visibility);
    assert!(!fs.signature);
    assert!(!fs.doc_comment);
    assert!(!fs.annotation);
    assert!(!fs.role);
}

#[test]
fn parse_trims_whitespace() {
    let fs = FieldSet::parse(" file , module , role ");
    assert!(fs.file);
    assert!(!fs.id);
    assert!(fs.module);
    assert!(!fs.span);
    assert!(!fs.snippet);
    assert!(!fs.visibility);
    assert!(!fs.signature);
    assert!(!fs.doc_comment);
    assert!(!fs.annotation);
    assert!(fs.role);
}

#[test]
fn parse_ignores_unknown_fields() {
    let fs = FieldSet::parse("file,unknown,id");
    assert!(fs.file);
    assert!(fs.id);
    assert!(!fs.module);
}

#[test]
fn parse_doc_comment_field() {
    let fs = FieldSet::parse("doc_comment");
    assert!(fs.doc_comment);
    assert!(!fs.file);
    assert!(!fs.signature);
}

#[test]
fn parse_annotation_field() {
    let fs = FieldSet::parse("annotation");
    assert!(fs.annotation);
    assert!(!fs.score);
    assert!(!fs.file);
    assert!(!fs.doc_comment);
}

#[test]
fn parse_score_field() {
    let fs = FieldSet::parse("score");
    assert!(fs.score);
    assert!(!fs.file);
    assert!(!fs.annotation);
}

#[test]
fn from_config_with_vec_of_strings() {
    let config_fields = vec![
        "file".to_string(),
        "signature".to_string(),
        "visibility".to_string(),
    ];
    let fs = FieldSet::from_config(&config_fields);
    assert!(fs.file);
    assert!(!fs.id);
    assert!(!fs.module);
    assert!(!fs.span);
    assert!(!fs.snippet);
    assert!(fs.visibility);
    assert!(fs.signature);
    assert!(!fs.doc_comment);
    assert!(!fs.annotation);
    assert!(!fs.role);
}

#[test]
fn from_config_empty_vec_gives_none() {
    let fs = FieldSet::from_config(&[]);
    // empty string parses as "none" essentially — no fields match
    assert!(!fs.file);
    assert!(!fs.id);
}
