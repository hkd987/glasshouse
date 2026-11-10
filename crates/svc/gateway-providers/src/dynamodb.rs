//! Amazon DynamoDB (ADR 0069): where agent hosts keep people's schedules,
//! so a schedule runs whether or not its person's computer is awake, and
//! on whichever host claims it first. AWS JSON 1.0, signed with SigV4 as
//! the host's instance role.
//!
//! Only what the schedules need: put, update and delete with a condition
//! (the claim that makes a run start once across hosts), get, and query
//! (an index of what's due).

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::http::{Http, Target};
use crate::s3::S3Credentials;
use crate::sigv4::{self, Credentials};

const CONTENT_TYPE: &str = "application/x-amz-json-1.0";
const SIGNING_NAME: &str = "dynamodb";
/// Pages a query reads before it stops: what's due is never that many.
const PAGES_MAX: usize = 50;

/// Why a call didn't do what it asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DynamoError {
    /// The condition didn't hold (`ConditionalCheckFailedException`): someone
    /// else got there first. Not a failure of the call.
    ConditionFailed,
    /// Anything else, said for people.
    Failed(String),
}

impl std::fmt::Display for DynamoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConditionFailed => f.write_str("someone else changed it first"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// A condition or update's expression and its names and values.
#[derive(Debug, Clone, Default)]
pub struct Expression<'a> {
    pub text: &'a str,
    /// `#name` → attribute name.
    pub names: Value,
    /// `:value` → DynamoDB attribute value (`{"S": "…"}`, `{"N": "1"}`).
    pub values: Value,
}

#[derive(Debug)]
pub struct DynamoDb {
    http: Http,
    region: String,
    base: String,
    host: String,
    credentials: S3Credentials,
}

impl DynamoDb {
    /// `region`'s endpoint, or `endpoint` (a VPC endpoint's name, or a
    /// stand-in on this machine's loopback address).
    pub fn new(
        region: &str,
        endpoint: Option<&str>,
        credentials: S3Credentials,
    ) -> Result<Self, String> {
        let base = endpoint.map_or_else(
            || format!("https://dynamodb.{region}.amazonaws.com"),
            |e| e.trim_end_matches('/').to_owned(),
        );
        let target = Target::parse(&format!("{base}/"))
            .ok_or_else(|| format!("{base:?} isn't an http(s) URL"))?;
        let http = if target.tls {
            Http::new()
        } else if crate::console::is_loopback(&target.host) {
            Http::loopback()
        } else {
            return Err(format!("the DynamoDB endpoint {base:?} must use HTTPS"));
        };
        Ok(Self {
            http: http.quick(),
            region: region.to_owned(),
            host: target.authority(),
            base,
            credentials,
        })
    }

    fn call(&self, operation: &str, body: &Value) -> Result<Value, DynamoError> {
        let now = OffsetDateTime::now_utc();
        let keys = self.credentials.keys(now).map_err(DynamoError::Failed)?;
        let body = body.to_string();
        let target = format!("DynamoDB_20120810.{operation}");
        let headers = [
            ("content-type", CONTENT_TYPE),
            ("x-amz-target", target.as_str()),
        ];
        let signed = sigv4::sign(
            &sigv4::Request {
                method: "POST",
                host: &self.host,
                canonical_path: "/",
                query: "",
                headers: &headers,
                body: body.as_bytes(),
            },
            &Credentials {
                access_key_id: keys.access_key_id.expose(),
                secret_access_key: keys.secret_access_key.expose(),
                session_token: keys
                    .session_token
                    .as_ref()
                    .map(|t| t.expose())
                    .filter(|t| !t.is_empty()),
            },
            &self.region,
            SIGNING_NAME,
            now,
        );
        let mut all: Vec<(&str, &str)> = headers.to_vec();
        all.extend(signed.iter().map(|(k, v)| (*k, v.as_str())));
        let reply = self
            .http
            .post(&format!("{}/", self.base), &all, body.as_bytes())
            .map_err(|e| DynamoError::Failed(format!("DynamoDB can't be reached: {e}")))?;
        let parsed: Value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
        if reply.status == 200 {
            return Ok(parsed);
        }
        let kind = parsed["__type"]
            .as_str()
            .and_then(|t| t.rsplit('#').next())
            .unwrap_or_default();
        if kind == "ConditionalCheckFailedException" {
            return Err(DynamoError::ConditionFailed);
        }
        let message = parsed["message"]
            .as_str()
            .or_else(|| parsed["Message"].as_str())
            .unwrap_or("no details");
        Err(DynamoError::Failed(format!(
            "DynamoDB {operation}: HTTP {}, {kind}: {message}",
            reply.status
        )))
    }

    fn with_expression(body: &mut Value, field: &str, expression: &Expression<'_>) {
        if expression.text.is_empty() {
            return;
        }
        body[field] = json!(expression.text);
        merge(body, "ExpressionAttributeNames", &expression.names);
        merge(body, "ExpressionAttributeValues", &expression.values);
    }

    /// Writes `item` whole, if `condition` holds.
    pub fn put(
        &self,
        table: &str,
        item: &Value,
        condition: &Expression<'_>,
    ) -> Result<(), DynamoError> {
        let mut body = json!({ "TableName": table, "Item": item });
        Self::with_expression(&mut body, "ConditionExpression", condition);
        self.call("PutItem", &body).map(drop)
    }

    /// Changes the item at `key` as `update` says, if `condition` holds.
    pub fn update(
        &self,
        table: &str,
        key: &Value,
        update: &Expression<'_>,
        condition: &Expression<'_>,
    ) -> Result<(), DynamoError> {
        let mut body = json!({ "TableName": table, "Key": key });
        Self::with_expression(&mut body, "UpdateExpression", update);
        Self::with_expression(&mut body, "ConditionExpression", condition);
        self.call("UpdateItem", &body).map(drop)
    }

    /// Removes the item at `key`, if `condition` holds.
    pub fn delete(
        &self,
        table: &str,
        key: &Value,
        condition: &Expression<'_>,
    ) -> Result<(), DynamoError> {
        let mut body = json!({ "TableName": table, "Key": key });
        Self::with_expression(&mut body, "ConditionExpression", condition);
        self.call("DeleteItem", &body).map(drop)
    }

    /// The item at `key`, read strongly consistently.
    pub fn get(&self, table: &str, key: &Value) -> Result<Option<Value>, DynamoError> {
        let body = json!({ "TableName": table, "Key": key, "ConsistentRead": true });
        let found = self.call("GetItem", &body)?;
        Ok(found.get("Item").cloned())
    }

    /// Every item `key` selects, from the table or `index`, all pages.
    pub fn query(
        &self,
        table: &str,
        index: Option<&str>,
        key: &Expression<'_>,
    ) -> Result<Vec<Value>, DynamoError> {
        let mut items = Vec::new();
        let mut start: Option<Value> = None;
        for _ in 0..PAGES_MAX {
            let mut body = json!({ "TableName": table });
            if let Some(index) = index {
                body["IndexName"] = json!(index);
            }
            Self::with_expression(&mut body, "KeyConditionExpression", key);
            if let Some(start) = start.take() {
                body["ExclusiveStartKey"] = start;
            }
            let page = self.call("Query", &body)?;
            if let Some(found) = page["Items"].as_array() {
                items.extend(found.iter().cloned());
            }
            match page.get("LastEvaluatedKey") {
                Some(next) if !next.is_null() => start = Some(next.clone()),
                _ => return Ok(items),
            }
        }
        Ok(items)
    }
}

fn merge(body: &mut Value, field: &str, add: &Value) {
    let Some(add) = add.as_object() else { return };
    if add.is_empty() {
        return;
    }
    if !body[field].is_object() {
        body[field] = json!({});
    }
    if let Some(into) = body[field].as_object_mut() {
        for (k, v) in add {
            into.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake;

    fn client(url: &str) -> DynamoDb {
        DynamoDb::new(
            "us-east-1",
            Some(url),
            S3Credentials::new("AKIDEXAMPLE", "secret", Some("token")),
        )
        .unwrap()
    }

    #[test]
    fn adr0069_a_claim_is_a_conditional_update_signed_for_dynamodb() {
        let (url, got) = fake::serve(200, "{}");
        client(&url)
            .update(
                "schedules",
                &json!({"owner": {"S": "p1"}, "id": {"S": "s1"}}),
                &Expression {
                    text: "SET next_run = :next",
                    names: json!({}),
                    values: json!({":next": {"N": "200"}}),
                },
                &Expression {
                    text: "next_run = :seen",
                    names: json!({}),
                    values: json!({":seen": {"N": "100"}}),
                },
            )
            .unwrap();
        let sent = got.recv().unwrap();
        let head = sent.head.to_ascii_lowercase();
        assert!(
            head.contains("x-amz-target: dynamodb_20120810.updateitem\r\n"),
            "{head}"
        );
        assert!(head.contains("content-type: application/x-amz-json-1.0\r\n"));
        assert!(head.contains("/us-east-1/dynamodb/aws4_request"), "{head}");
        assert!(head.contains("x-amz-security-token: token\r\n"));
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["ConditionExpression"], "next_run = :seen");
        assert_eq!(body["ExpressionAttributeValues"][":seen"]["N"], "100");
        assert_eq!(body["ExpressionAttributeValues"][":next"]["N"], "200");
    }

    #[test]
    fn adr0069_losing_a_claim_is_its_own_answer() {
        let (url, _got) = fake::serve(
            400,
            r#"{"__type":"com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException","message":"The conditional request failed"}"#,
        );
        let lost = client(&url).put("schedules", &json!({}), &Expression::default());
        assert_eq!(lost, Err(DynamoError::ConditionFailed));
        let (url, _got) = fake::serve(
            400,
            r#"{"__type":"com.amazonaws.dynamodb.v20120810#ResourceNotFoundException","message":"Requested resource not found"}"#,
        );
        let missing = client(&url).get("schedules", &json!({})).unwrap_err();
        assert!(
            missing.to_string().contains("ResourceNotFoundException"),
            "{missing}"
        );
        assert!(
            DynamoDb::new(
                "us-east-1",
                Some("http://dynamodb.example.com"),
                S3Credentials::new("a", "b", None)
            )
            .is_err()
        );
    }

    #[test]
    fn adr0069_a_query_reads_every_page() {
        let (url, got) = fake::serve_each(vec![
            (
                200,
                r#"{"Items":[{"id":{"S":"s1"}}],"LastEvaluatedKey":{"id":{"S":"s1"}}}"#,
            ),
            (200, r#"{"Items":[{"id":{"S":"s2"}}]}"#),
        ]);
        let items = client(&url)
            .query(
                "schedules",
                Some("due"),
                &Expression {
                    text: "#q = :q AND next_run <= :now",
                    names: json!({"#q": "queue"}),
                    values: json!({":q": {"S": "due"}, ":now": {"N": "150"}}),
                },
            )
            .unwrap();
        assert_eq!(items.len(), 2);
        let _first = got.recv().unwrap();
        let second: Value = serde_json::from_str(&got.recv().unwrap().body).unwrap();
        assert_eq!(second["ExclusiveStartKey"]["id"]["S"], "s1");
        assert_eq!(second["IndexName"], "due");
    }
}
