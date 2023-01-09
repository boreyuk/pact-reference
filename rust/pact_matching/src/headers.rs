//! Matching functions for headers

use std::collections::HashMap;
use std::iter::FromIterator;

use maplit::*;

use pact_models::headers::PARAMETERISED_HEADERS;
use pact_models::matchingrules::MatchingRule;
use pact_models::path_exp::DocPath;
use tracing::instrument;

use crate::{matchers, MatchingContext, Mismatch};
use crate::matchers::Matches;

fn strip_whitespace<'a, T: FromIterator<&'a str>>(val: &'a str, split_by: &'a str) -> T {
  val.split(split_by).map(|v| v.trim()).filter(|v| !v.is_empty()).collect()
}

fn parse_charset_parameters(parameters: &[&str]) -> HashMap<String, String> {
  parameters.iter().map(|v| v.split_once('=')
    .map(|(k, v)| (k.trim(), v.trim())))
    .fold(HashMap::new(), |mut map, name_value| {
      if let Some((name, value)) = name_value {
        map.insert(name.to_string(), value.to_string());
      }
      map
    })
}

pub(crate) fn match_parameter_header(expected: &str, actual: &str, header: &str, value_type: &str) -> Result<(), Vec<String>> {
  let expected_values: Vec<&str> = strip_whitespace(expected, ";");
  let actual_values: Vec<&str> = strip_whitespace(actual, ";");

  let expected_parameters = expected_values.as_slice().split_first().unwrap_or((&"", &[]));
  let actual_parameters = actual_values.as_slice().split_first().unwrap_or((&"", &[]));
  let header_mismatch = format!("Expected {} '{}' to have value '{}' but was '{}'", value_type, header, expected, actual);

  let mut mismatches = vec![];
  if expected_parameters.0 == actual_parameters.0 {
    let expected_parameter_map = parse_charset_parameters(expected_parameters.1);
    let actual_parameter_map = parse_charset_parameters(actual_parameters.1);
    for (k, v) in expected_parameter_map {
      if actual_parameter_map.contains_key(&k) {
        if v.to_ascii_lowercase() != actual_parameter_map.get(&k).unwrap().to_ascii_lowercase() {
          mismatches.push(header_mismatch.clone());
        }
      } else {
        mismatches.push(header_mismatch.clone());
      }
    }
  } else {
    mismatches.push(header_mismatch);
  }

  if mismatches.is_empty() {
    Ok(())
  } else {
    Err(mismatches)
  }
}

#[instrument(level = "trace")]
pub(crate) fn match_header_value(
  key: &str,
  index: usize,
  expected: &str,
  actual: &str,
  context: &dyn MatchingContext,
  single_value: bool
) -> Result<(), Vec<Mismatch>> {
  let path = DocPath::root().join(key);
  let indexed_path = path.join(index.to_string());
  let expected: String = strip_whitespace(expected, ",");
  let actual: String = strip_whitespace(actual, ",");

  let matcher_result = if context.matcher_is_defined(&path) {
    let result = matchers::match_values(&path, &context.select_best_matcher(&path), &expected, &actual);
    if single_value {
      result
    } else {
      result.map_err(|err| err.iter().map(|e| format!("{} for value at index {}", e, index)).collect())
    }
  } else if context.matcher_is_defined(&indexed_path) {
    let result = matchers::match_values(&indexed_path, &context.select_best_matcher(&indexed_path), &expected, &actual);
    if single_value {
      result
    } else {
      result.map_err(|err| err.iter().map(|e| format!("{} for value at index {}", e, index)).collect())
    }
  } else if PARAMETERISED_HEADERS.contains(&key.to_lowercase().as_str()) {
    match_parameter_header(expected.as_str(), actual.as_str(), key, "header")
  } else {
    Matches::matches_with(&expected, &actual, &MatchingRule::Equality, false)
      .map_err(|err| {
        if single_value {
          vec![format!("{}", err)]
        } else {
          vec![format!("{} for value at index {}", err, index)]
        }
      })
  };

  matcher_result.map_err(|messages| {
    messages.iter().map(|message| {
      Mismatch::HeaderMismatch {
        key: key.to_string(),
        expected: expected.to_string(),
        actual: actual.to_string(),
        mismatch: format!("Mismatch with header '{}': {}", key, message)
      }
    }).collect()
  })
}

fn find_entry<T>(map: &HashMap<String, T>, key: &str) -> Option<(String, T)> where T: Clone {
  match map.keys().find(|k| k.to_lowercase() == key.to_lowercase() ) {
    Some(k) => map.get(k).map(|v| (key.to_string(), v.clone()) ),
    None => None
  }
}

fn match_header_maps(
  expected: HashMap<String, Vec<String>>,
  actual: HashMap<String, Vec<String>>,
  context: &dyn MatchingContext
) -> HashMap<String, Vec<Mismatch>> {
  let mut result = hashmap!{};
  for (key, value) in &expected {
    match find_entry(&actual, key) {
      Some((_, actual_values)) => if value.is_empty() && !actual_values.is_empty() {
        result.insert(key.clone(), vec![Mismatch::HeaderMismatch { key: key.clone(),
          expected: "".to_string(),
          actual: format!("{}", actual_values.join(", ")),
          mismatch: format!("Expected an empty header '{}' but actual value was '{}'", key, actual_values.join(", ")) }]);
      } else {
        let mut mismatches = vec![];

        // Special case when the headers only have 1 value to improve messaging
        if value.len() == 1 && actual_values.len() == 1 {
          let comparison_result = match_header_value(key, 0, value.first().unwrap(),
            actual_values.first().unwrap(), context, true).err().unwrap_or_default();
          mismatches.extend(comparison_result.iter().cloned());
        } else {
          for (index, val) in value.iter().enumerate() {
            if let Some(actual_value) = actual_values.get(index) {
              let comparison_result = match_header_value(key, index, val,
                actual_value, context, false).err().unwrap_or_default();
              mismatches.extend(comparison_result.iter().cloned());
            } else {
              mismatches.push(Mismatch::HeaderMismatch {
                key: key.clone(),
                expected: val.clone(),
                actual: "".to_string(),
                mismatch: format!("Mismatch with header '{}': Expected value '{}' at index {} but was missing (actual has {} value(s))",
                                  key, val, index, actual_values.len())
              });
            }
          }
        }

        result.insert(key.clone(), mismatches);
      },
      None => {
        result.insert(key.clone(), vec![Mismatch::HeaderMismatch { key: key.clone(),
          expected: format!("{:?}", value.join(", ")),
          actual: "".to_string(),
          mismatch: format!("Expected header '{}' but was missing", key) }]);
      }
    }
  }
  result
}

/// Matches the actual headers to the expected ones.
pub fn match_headers(
  expected: Option<HashMap<String, Vec<String>>>,
  actual: Option<HashMap<String, Vec<String>>>,
  context: &(dyn MatchingContext + Send + Sync)
) -> HashMap<String, Vec<Mismatch>> {
  match (actual, expected) {
    (Some(aqm), Some(eqm)) => match_header_maps(eqm, aqm, context),
    (Some(_), None) => hashmap!{},
    (None, Some(eqm)) => eqm.iter().map(|(key, value)| {
      (key.clone(), vec![Mismatch::HeaderMismatch { key: key.clone(),
        expected: format!("{:?}", value.join(", ")),
        actual: "".to_string(),
        mismatch: format!("Expected header '{}' but was missing", key) }])
    }).collect(),
    (None, None) => hashmap!{}
  }
}

#[cfg(test)]
mod tests {
  use expectest::prelude::*;
  use maplit::*;

  use pact_models::matchingrules;
  use pact_models::matchingrules::MatchingRule;

  use crate::{CoreMatchingContext, DiffConfig, Mismatch};
  use crate::headers::{match_header_value, match_headers, parse_charset_parameters};

  #[test]
  fn matching_headers_be_true_when_headers_are_equal() {
    let mismatches = match_header_value("HEADER", 0, "HEADER", "HEADER",
      &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn matching_headers_be_false_when_headers_are_not_equal() {
    let mismatches = match_header_value("HEADER", 0, "HEADER", "HEADER2",
      &CoreMatchingContext::default(), true
    ).unwrap_err();
    expect!(mismatches.iter()).to_not(be_empty());
    assert_eq!(mismatches[0], Mismatch::HeaderMismatch {
      key: "HEADER".to_string(),
      expected: "HEADER".to_string(),
      actual: "HEADER2".to_string(),
      mismatch: "".to_string()
    });
  }

  #[test]
  fn mismatch_message_generated_when_headers_are_not_equal() {
    let mismatches = match_header_value("HEADER", 0, "HEADER_VALUE", "HEADER2",
      &CoreMatchingContext::default(), true
    );

    match mismatches.unwrap_err()[0] {
      Mismatch::HeaderMismatch { ref mismatch, .. } =>
        assert_eq!(mismatch, "Mismatch with header 'HEADER': Expected 'HEADER_VALUE' to be equal to 'HEADER2'"),
      _ => panic!("Unexpected mismatch response")
    }
  }

  #[test]
  fn matching_headers_exclude_whitespaces() {
    let mismatches = match_header_value("HEADER", 0, "HEADER1, HEADER2,   3",
      "HEADER1,HEADER2,3", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn matching_headers_includes_whitespaces_within_a_value() {
    let mismatches = match_header_value("HEADER", 0, "HEADER 1, \tHEADER 2,\n3",
     "HEADER 1,HEADER 2,3", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn content_type_header_matches_when_headers_are_equal() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/json;charset=UTF-8",
      "application/json; charset=UTF-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn content_type_header_does_not_match_when_headers_are_not_equal() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/pdf;charset=UTF-8",
      "application/json;charset=UTF-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn content_type_header_does_not_match_when_expected_is_empty() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "",
      "application/json;charset=UTF-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn content_type_header_does_not_match_when_actual_is_empty() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/pdf;charset=UTF-8",
      "", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn content_type_header_does_not_match_when_charsets_are_not_equal() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/json;charset=UTF-8",
      "application/json;charset=UTF-16", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn content_type_header_does_match_when_charsets_are_different_case() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/json;charset=UTF-8",
      "application/json;charset=utf-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn content_type_header_does_not_match_when_charsets_other_parameters_not_equal() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/json;declaration=\"<950118.AEB0@XIson.com>\"",
      "application/json;charset=UTF-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn content_type_header_does_match_when_charsets_is_missing_from_expected_header() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "application/json",
      "application/json;charset=UTF-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn mismatched_header_description_reports_content_type_mismatches_correctly() {
    let mismatches = match_header_value("CONTENT-TYPE", 0, "CONTENT-TYPE-VALUE", "HEADER2",
      &CoreMatchingContext::default(), true
    );

    match mismatches.unwrap_err()[0] {
      Mismatch::HeaderMismatch { ref mismatch, .. } =>
        assert_eq!(mismatch, "Mismatch with header 'CONTENT-TYPE': Expected header 'CONTENT-TYPE' to have value 'CONTENT-TYPE-VALUE' but was 'HEADER2'"),
      _ => panic!("Unexpected mismatch response")
    }
  }

  #[test]
  fn accept_header_matches_when_headers_are_equal() {
    let mismatches = match_header_value("ACCEPT", 0, "application/hal+json;charset=utf-8",
      "application/hal+json;charset=utf-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn accept_header_does_not_match_when_actual_is_empty() {
    let mismatches = match_header_value("ACCEPT", 0, "application/hal+json",
      "", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn accept_header_does_match_when_charset_is_missing_from_expected_header() {
    let mismatches = match_header_value("ACCEPT", 0, "application/hal+json",
      "application/hal+json;charset=utf-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn accept_header_does_not_match_when_charsets_are_not_equal() {
    let mismatches = match_header_value("ACCEPT", 0, "application/hal+json;charset=utf-8",
      "application/hal+json;charset=utf-16", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_err());
  }

  #[test]
  fn accept_header_does_match_when_charsets_are_different_case() {
    let mismatches = match_header_value("ACCEPT", 0, "application/hal+json;charset=utf-8",
      "application/hal+json;charset=UTF-8", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn mismatched_header_description_reports_accept_header_mismatches_correctly() {
    let mismatches = match_header_value("ACCEPT", 0, "ACCEPT-VALUE", "HEADER2",
      &CoreMatchingContext::default(), true
    );
    match mismatches.unwrap_err()[0] {
      Mismatch::HeaderMismatch { ref mismatch, .. } =>
        assert_eq!(mismatch, "Mismatch with header 'ACCEPT': Expected header 'ACCEPT' to have value 'ACCEPT-VALUE' but was 'HEADER2'"),
      _ => panic!("Unexpected mismatch response")
    }
  }

  #[test]
  fn accept_header_matching_with_multiple_values() {
    let expected = Some(hashmap! { "accept".to_string() => vec!["application/json".to_string(), "application/hal+json".to_string()] });
    let actual = Some(hashmap! { "accept".to_string() => vec!["application/json".to_string(), "application/hal+json".to_string()] });
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    expect!(result.values().flatten()).to(be_empty());
  }

  #[test]
  fn matching_headers_be_true_when_headers_match_by_matcher() {
    let context = CoreMatchingContext::new(
      DiffConfig::AllowUnexpectedKeys,
      &matchingrules! {
        "header" => {
          "HEADER" => [ MatchingRule::Regex("\\w+".to_string()) ]
        }
      }.rules_for_category("header").unwrap_or_default(), &hashmap!{}
    );
    let mismatches = match_header_value("HEADER", 0, "HEADERX", "HEADERY", &context, true);
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn matching_headers_be_false_when_headers_do_not_match_by_matcher() {
    let context = CoreMatchingContext::new(
      DiffConfig::AllowUnexpectedKeys,
      &matchingrules! {
          "header" => {
              "HEADER" => [ MatchingRule::Regex("\\d+".to_string()) ]
          }
        }.rules_for_category("header").unwrap_or_default(), &hashmap!{}
    );
    let mismatches = match_header_value(&"HEADER".to_string(), 0,
      &"HEADER".to_string(), &"HEADER".to_string(), &context, true);
    expect!(mismatches).to(be_err().value(vec![ Mismatch::HeaderMismatch {
      key: "HEADER".to_string(),
      expected: "HEADER".to_string(),
      actual: "HEADER".to_string(),
      mismatch: String::default(),
    } ]));
  }

  #[test]
  fn match_header_value_does_match_when_not_well_formed() {
    let mismatches = match_header_value("content-type", 0, "application/json",
      "application/json;", &CoreMatchingContext::default(), true
    );
    expect!(mismatches).to(be_ok());
  }

  #[test]
  fn parse_charset_parameters_test() {
    expect!(parse_charset_parameters(&[])).to(be_equal_to(hashmap!{}));
    expect!(parse_charset_parameters(&[""])).to(be_equal_to(hashmap!{}));
    expect!(parse_charset_parameters(&["a"])).to(be_equal_to(hashmap!{}));
    expect!(parse_charset_parameters(&["a="])).to(be_equal_to(hashmap!{ "a".to_string() => String::default() }));
    expect!(parse_charset_parameters(&["a=b"])).to(be_equal_to(hashmap!{ "a".to_string() => "b".to_string() }));
    expect!(parse_charset_parameters(&["a=b", "c=d"])).to(be_equal_to(hashmap!{
      "a".to_string() => "b".to_string(),
      "c".to_string() => "d".to_string()
    }));
  }

  // Issue #238
  #[test_log::test]
  fn matching_headers_with_an_indexed_path() {
    let context = CoreMatchingContext::new(
      DiffConfig::AllowUnexpectedKeys,
      &matchingrules! {
        "header" => {
          "HEADER[0]" => [ MatchingRule::Regex("\\w+".to_string()) ]
        }
      }.rules_for_category("header").unwrap_or_default(), &hashmap!{}
    );
    let mismatches = match_header_value("HEADER", 0, "HEADERX", "HEADERY", &context, true);
    expect!(mismatches).to(be_ok());
  }

  #[test_log::test]
  fn match_headers_returns_nothing_if_there_are_no_headers() {
    let expected = None;
    let actual = None;
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    expect!(result.values().flatten()).to(be_empty());
  }

  #[test_log::test]
  fn match_headers_applies_matching_rules_when_header_name_has_an_underscore() {
    let expected = hashmap! { "user_id".to_string() => vec!["1".to_string()] };
    let actual = hashmap! { "user_id".to_string() => vec!["2".to_string()] };
    let rules = matchingrules! {
    "header" => { "user_id" => [ MatchingRule::Regex("^[0-9]+$".to_string()) ] }
  };
    let context = CoreMatchingContext::new(
      DiffConfig::AllowUnexpectedKeys,
      &rules.rules_for_category("header").unwrap_or_default(), &hashmap!{}
    );
    let result = match_headers(Some(expected), Some(actual), &context);
    expect!(result.values().flatten()).to(be_empty());
  }

  #[test]
  fn match_headers_returns_no_mismatch_if_there_is_no_expected_header_and_we_allow_unexpected_keys() {
    let expected = None;
    let actual = Some(hashmap!{
    "a".to_string() => vec!["b".to_string()]
  });
    let result = match_headers(expected, actual,
                               &CoreMatchingContext::with_config(DiffConfig::AllowUnexpectedKeys));
    let mismatches: Vec<Mismatch> = result.values().flatten().cloned().collect();
    expect!(mismatches.iter()).to(be_empty());
  }

  #[test]
  fn match_headers_returns_a_mismatch_if_there_is_no_actual_headers() {
    let expected = Some(hashmap! {
      "a".to_string() => vec!["b".to_string()]
    });
    let actual = None;
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    let mismatches: Vec<Mismatch> = result.values().flatten().cloned().collect();
    expect!(mismatches.iter()).to_not(be_empty());
    assert_eq!(mismatches[0], Mismatch::HeaderMismatch {
      key: "a".to_string(),
      expected: "\"b\"".to_string(),
      actual: "".to_string(),
      mismatch: "Expected header 'a' but was missing".to_string()
    });
  }

  #[test]
  fn match_headers_returns_a_mismatch_if_there_is_an_expected_header_that_is_not_received() {
    let expected = Some(hashmap!{
      "a".to_string() => vec!["b".to_string()],
      "c".to_string() => vec!["d".to_string()]
    });
    let actual = Some(hashmap!{
      "c".to_string() => vec!["d".to_string()]
    });
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    let mismatches: Vec<Mismatch> = result.values().flatten().cloned().collect();
    expect!(mismatches.iter()).to_not(be_empty());
    assert_eq!(mismatches[0], Mismatch::HeaderMismatch {
      key: "a".to_string(),
      expected: "\"b\"".to_string(),
      actual: "".to_string(),
      mismatch: "Expected header 'a' but was missing".to_string(),
    });
  }

  #[test]
  fn match_headers_returns_a_mismatch_if_there_is_an_empty_expected_header_and_a_non_empty_actual() {
    let expected = Some(hashmap!{
      "a".to_string() => vec!["b".to_string()],
      "c".to_string() => vec![]
    });
    let actual = Some(hashmap!{
      "a".to_string() => vec!["b".to_string()],
      "c".to_string() => vec!["d".to_string()]
    });
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    let mismatches: Vec<Mismatch> = result.values().flatten().cloned().collect();
    expect!(mismatches.iter()).to_not(be_empty());
    assert_eq!(mismatches[0], Mismatch::HeaderMismatch {
      key: "c".to_string(),
      expected: "".to_string(),
      actual: "d".to_string(),
      mismatch: "Expected an empty header 'c' but actual value was 'd'".to_string(),
    });
  }

  #[test]
  fn match_headers_returns_a_mismatch_if_the_header_values_have_different_lengths() {
    let expected = Some(hashmap!{
      "a".to_string() => vec!["b".to_string()],
      "c".to_string() => vec!["d".to_string(), "e".to_string()]
    });
    let actual = Some(hashmap!{
      "a".to_string() => vec!["b".to_string()],
      "c".to_string() => vec!["d".to_string()]
    });
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    let mismatches: Vec<Mismatch> = result.values().flatten().cloned().collect();
    expect!(mismatches.len()).to(be_equal_to(1));
    expect!(mismatches[0].clone()).to(be_equal_to(Mismatch::HeaderMismatch {
      key: "c".to_string(),
      expected: "e".to_string(),
      actual: "".to_string(),
      mismatch: "Mismatch with header 'c': Expected value 'e' at index 1".to_string(),
    }));

    let expected = Some(hashmap!{
      "c".to_string() => vec!["d".to_string(), "e".to_string()]
    });
    let actual = Some(hashmap!{
      "c".to_string() => vec!["e".to_string()]
    });
    let result = match_headers(expected, actual, &CoreMatchingContext::default());
    let mismatches: Vec<Mismatch> = result.values().flatten().cloned().collect();
    expect!(mismatches.len()).to(be_equal_to(2));
    expect!(mismatches[0].clone()).to(be_equal_to(Mismatch::HeaderMismatch {
      key: "c".to_string(),
      expected: "d".to_string(),
      actual: "e".to_string(),
      mismatch: "Mismatch with header 'c': Expected 'd' to be equal to 'e' for value at index 0".to_string(),
    }));
    expect!(mismatches[1].clone()).to(be_equal_to(Mismatch::HeaderMismatch {
      key: "c".to_string(),
      expected: "e".to_string(),
      actual: "".to_string(),
      mismatch: "Mismatch with header 'c': Expected value 'e' at index 1 but was missing (actual has 1 value(s))".to_string(),
    }));
  }

  #[test_log::test]
  fn match_query_with_min_type_matching_rules() {
    let expected = hashmap! { "id".to_string() => vec!["1".to_string(), "2".to_string()] };
    let actual = hashmap! { "id".to_string() => vec![
      "1".to_string(),
      "2".to_string(),
      "3".to_string(),
      "4".to_string()
    ]};
    let rules = matchingrules! {
      "header" => { "id" => [ MatchingRule::MinType(2) ] }
    };
    let context = CoreMatchingContext::new(
      DiffConfig::AllowUnexpectedKeys,
      &rules.rules_for_category("header").unwrap_or_default(), &hashmap!{}
    );
    let result = match_headers(Some(expected), Some(actual), &context);
    expect!(result.values().flatten()).to(be_empty());
  }
}
