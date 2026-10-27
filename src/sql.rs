// SPDX-License-Identifier: AGPL-3.0-only
//! Small SeaWorld SQL front end. It translates to the existing native query and
//! transaction APIs; it never reads or writes storage independently.

use std::collections::BTreeMap;

use crate::{
    CollectionId, CompareOp, Database, Document, DocumentId, Error, FieldPath, Filter, Query,
    QueryPlan, Result, Value,
};

pub const MAX_SQL_BYTES: usize = 64 * 1024;
pub const MAX_SQL_TOKENS: usize = 4_096;
const MAX_SQL_STRING_BYTES: usize = 16 * 1024;
const MAX_SQL_VALUE_DEPTH: usize = 64;

/// One projected SQL result row. Projected field paths become keys in `values`.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlRow {
    pub id: DocumentId,
    pub values: Document,
}

/// Result of one supported SQL statement.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlResult {
    Rows(Vec<SqlRow>),
    AffectedRows(usize),
    CollectionCreated(CollectionId),
    Plan(QueryPlan),
}

pub(crate) fn execute(
    database: &mut Database,
    source: &str,
    parameters: &[Value],
) -> Result<SqlResult> {
    let statement = Parser::new(source, parameters)?.parse_statement()?;
    execute_statement(database, statement)
}

#[derive(Debug)]
enum Statement {
    Select {
        collection: String,
        projection: Option<Vec<FieldPath>>,
        filter: Option<(FieldPath, CompareOp, Value)>,
    },
    Insert {
        collection: String,
        document: Document,
    },
    Update {
        collection: String,
        assignments: Vec<(FieldPath, Value)>,
        filter: (FieldPath, CompareOp, Value),
    },
    Delete {
        collection: String,
        filter: (FieldPath, CompareOp, Value),
    },
    CreateCollection(String),
    Explain(Box<Statement>),
}

fn execute_statement(database: &mut Database, statement: Statement) -> Result<SqlResult> {
    match statement {
        Statement::Select {
            collection,
            projection,
            filter,
        } => {
            let query = Query::new(&collection, make_filter(filter));
            let rows = database
                .execute(&query)?
                .into_iter()
                .map(|row| {
                    let values = match &projection {
                        None => row.document.clone(),
                        Some(paths) => {
                            let mut selected = BTreeMap::new();
                            for path in paths {
                                if let Some(value) = path.get(row.document) {
                                    selected.insert(path.to_string(), value.clone());
                                }
                            }
                            Document::new(selected)
                        }
                    };
                    SqlRow { id: row.id, values }
                })
                .collect();
            Ok(SqlResult::Rows(rows))
        }
        Statement::Insert {
            collection,
            document,
        } => {
            database.insert(&collection, document)?;
            Ok(SqlResult::AffectedRows(1))
        }
        Statement::Update {
            collection,
            assignments,
            filter,
        } => {
            let query = Query::new(&collection, make_filter(Some(filter)));
            let matches: Vec<_> = database
                .execute(&query)?
                .into_iter()
                .map(|row| (row.id, row.document.clone()))
                .collect();
            let mut transaction = database.begin_transaction()?;
            for (id, document) in &matches {
                let mut updated = document.clone();
                for (path, value) in &assignments {
                    updated = assign_path(updated, path, value.clone())?;
                }
                if !transaction.update(&collection, *id, updated)? {
                    return Err(Error::Integrity(
                        "SQL update lost a document between query and transaction".into(),
                    ));
                }
            }
            let count = matches.len();
            if count == 0 {
                transaction.rollback();
            } else {
                transaction.commit()?;
            }
            Ok(SqlResult::AffectedRows(count))
        }
        Statement::Delete { collection, filter } => {
            let query = Query::new(&collection, make_filter(Some(filter)));
            let ids: Vec<_> = database
                .execute(&query)?
                .into_iter()
                .map(|row| row.id)
                .collect();
            let mut transaction = database.begin_transaction()?;
            for id in &ids {
                if !transaction.delete(&collection, *id)? {
                    return Err(Error::Integrity(
                        "SQL delete lost a document between query and transaction".into(),
                    ));
                }
            }
            let count = ids.len();
            if count == 0 {
                transaction.rollback();
            } else {
                transaction.commit()?;
            }
            Ok(SqlResult::AffectedRows(count))
        }
        Statement::CreateCollection(name) => Ok(SqlResult::CollectionCreated(
            database.create_collection(&name)?,
        )),
        Statement::Explain(statement) => match *statement {
            Statement::Select {
                collection, filter, ..
            } => Ok(SqlResult::Plan(
                database.explain(&Query::new(collection, make_filter(filter)))?,
            )),
            _ => Err(Error::Query("EXPLAIN currently accepts SELECT only".into())),
        },
    }
}

fn make_filter(filter: Option<(FieldPath, CompareOp, Value)>) -> Filter {
    match filter {
        Some((path, op, value)) => Filter::Compare { path, op, value },
        None => Filter::All,
    }
}

fn assign_path(mut document: Document, path: &FieldPath, value: Value) -> Result<Document> {
    fn assign(fields: &mut BTreeMap<String, Value>, parts: &[String], value: Value) -> Result<()> {
        let (first, rest) = parts
            .split_first()
            .ok_or_else(|| Error::Query("field path is empty".into()))?;
        if rest.is_empty() {
            fields.insert(first.clone(), value);
            return Ok(());
        }
        let nested = fields
            .entry(first.clone())
            .or_insert_with(|| Value::Object(BTreeMap::new()));
        let Value::Object(nested_fields) = nested else {
            return Err(Error::Query(format!(
                "cannot assign nested field through non-object {first:?}"
            )));
        };
        assign(nested_fields, rest, value)
    }
    assign(document.fields_mut_for_sql(), path.segments(), value)?;
    Ok(document)
}

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Word(String),
    String(String),
    Number(String),
    Operator(String),
    Question,
    Star,
    Comma,
    Dot,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Colon,
    Semicolon,
    End,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    offset: usize,
}

struct Parser<'a> {
    tokens: Vec<Token>,
    cursor: usize,
    parameters: &'a [Value],
    parameter_cursor: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, parameters: &'a [Value]) -> Result<Self> {
        if source.len() > MAX_SQL_BYTES {
            return Err(Error::Limit(format!(
                "SQL statement exceeds {MAX_SQL_BYTES} bytes"
            )));
        }
        Ok(Self {
            tokens: lex(source)?,
            cursor: 0,
            parameters,
            parameter_cursor: 0,
        })
    }

    fn parse_statement(mut self) -> Result<Statement> {
        let explain = self.eat_keyword("EXPLAIN");
        let statement = if self.eat_keyword("SELECT") {
            self.parse_select_after_keyword()?
        } else if self.eat_keyword("INSERT") {
            self.expect_keyword("INTO")?;
            let collection = self.identifier()?;
            self.expect_keyword("VALUES")?;
            let value = self.value(0)?;
            let Value::Object(fields) = value else {
                return Err(self.error("INSERT VALUES requires a document object"));
            };
            Statement::Insert {
                collection,
                document: Document::new(fields),
            }
        } else if self.eat_keyword("UPDATE") {
            let collection = self.identifier()?;
            self.expect_keyword("SET")?;
            let mut assignments = Vec::new();
            loop {
                let path = self.field_path()?;
                self.expect_operator("=")?;
                let value = self.value(0)?;
                if assignments.iter().any(|(existing, _)| existing == &path) {
                    return Err(self.error("UPDATE repeats a field assignment"));
                }
                assignments.push((path, value));
                if !self.eat_simple(TokenKind::Comma) {
                    break;
                }
                if assignments.len() >= 256 {
                    return Err(Error::Limit("SQL UPDATE has too many assignments".into()));
                }
            }
            self.expect_keyword("WHERE")?;
            let filter = self.predicate()?;
            Statement::Update {
                collection,
                assignments,
                filter,
            }
        } else if self.eat_keyword("DELETE") {
            self.expect_keyword("FROM")?;
            let collection = self.identifier()?;
            self.expect_keyword("WHERE")?;
            Statement::Delete {
                collection,
                filter: self.predicate()?,
            }
        } else if self.eat_keyword("CREATE") {
            self.expect_keyword("COLLECTION")?;
            Statement::CreateCollection(self.identifier()?)
        } else {
            return Err(self.error("expected SELECT, INSERT, UPDATE, DELETE, or CREATE COLLECTION"));
        };
        if explain && !matches!(&statement, Statement::Select { .. }) {
            return Err(self.error("EXPLAIN currently accepts SELECT only"));
        }
        let _ = self.eat_simple(TokenKind::Semicolon);
        if !matches!(self.current().kind, TokenKind::End) {
            return Err(self.error("unexpected token after statement"));
        }
        if self.parameter_cursor != self.parameters.len() {
            return Err(self.error("parameter count does not match placeholders"));
        }
        Ok(if explain {
            Statement::Explain(Box::new(statement))
        } else {
            statement
        })
    }

    fn parse_select_after_keyword(&mut self) -> Result<Statement> {
        let projection = if self.eat_simple(TokenKind::Star) {
            None
        } else {
            let mut paths = vec![self.field_path()?];
            while self.eat_simple(TokenKind::Comma) {
                if paths.len() >= 256 {
                    return Err(Error::Limit("SQL projection has too many fields".into()));
                }
                paths.push(self.field_path()?);
            }
            Some(paths)
        };
        self.expect_keyword("FROM")?;
        let collection = self.identifier()?;
        let filter = if self.eat_keyword("WHERE") {
            Some(self.predicate()?)
        } else {
            None
        };
        Ok(Statement::Select {
            collection,
            projection,
            filter,
        })
    }

    fn predicate(&mut self) -> Result<(FieldPath, CompareOp, Value)> {
        let path = self.field_path()?;
        let operator = match self.current().kind.clone() {
            TokenKind::Operator(operator) => {
                self.cursor += 1;
                operator
            }
            _ => return Err(self.error("expected a comparison operator")),
        };
        let op = match operator.as_str() {
            "=" => CompareOp::Eq,
            "!=" | "<>" => CompareOp::Ne,
            "<" => CompareOp::Lt,
            "<=" => CompareOp::Lte,
            ">" => CompareOp::Gt,
            ">=" => CompareOp::Gte,
            _ => return Err(self.error("unsupported comparison operator")),
        };
        Ok((path, op, self.value(0)?))
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_SQL_VALUE_DEPTH {
            return Err(Error::Limit("SQL document nesting depth exceeded".into()));
        }
        let token = self.current().clone();
        match token.kind {
            TokenKind::String(value) => {
                self.cursor += 1;
                Ok(Value::String(value))
            }
            TokenKind::Number(number) => {
                self.cursor += 1;
                if number.contains(['.', 'e', 'E']) {
                    let value = number
                        .parse::<f64>()
                        .map_err(|_| self.error_at(token.offset, "invalid floating-point value"))?;
                    Value::floating(value)
                } else {
                    number
                        .parse::<i64>()
                        .map(Value::Integer)
                        .map_err(|_| self.error_at(token.offset, "integer is out of range"))
                }
            }
            TokenKind::Question => {
                self.cursor += 1;
                let value = self.parameters.get(self.parameter_cursor).ok_or_else(|| {
                    self.error_at(token.offset, "missing bound value for ? placeholder")
                })?;
                self.parameter_cursor += 1;
                Ok(value.clone())
            }
            TokenKind::LeftBrace => self.object_value(depth + 1),
            TokenKind::LeftBracket => self.array_value(depth + 1),
            TokenKind::Word(word) if word.eq_ignore_ascii_case("NULL") => {
                self.cursor += 1;
                Ok(Value::Null)
            }
            TokenKind::Word(word) if word.eq_ignore_ascii_case("TRUE") => {
                self.cursor += 1;
                Ok(Value::Bool(true))
            }
            TokenKind::Word(word) if word.eq_ignore_ascii_case("FALSE") => {
                self.cursor += 1;
                Ok(Value::Bool(false))
            }
            _ => Err(self.error("expected a literal, document, array, or bound parameter")),
        }
    }

    fn object_value(&mut self, depth: usize) -> Result<Value> {
        self.expect_simple(TokenKind::LeftBrace)?;
        let mut fields = BTreeMap::new();
        if self.eat_simple(TokenKind::RightBrace) {
            return Ok(Value::Object(fields));
        }
        loop {
            let key = match self.current().kind.clone() {
                TokenKind::String(key) => {
                    self.cursor += 1;
                    key
                }
                _ => return Err(self.error("document keys must be quoted strings")),
            };
            self.expect_simple(TokenKind::Colon)?;
            let value = self.value(depth)?;
            if fields.insert(key.clone(), value).is_some() {
                return Err(self.error("document contains a duplicate key"));
            }
            if self.eat_simple(TokenKind::RightBrace) {
                break;
            }
            self.expect_simple(TokenKind::Comma)?;
            if fields.len() >= 4_096 {
                return Err(Error::Limit("SQL document contains too many fields".into()));
            }
        }
        Ok(Value::Object(fields))
    }

    fn array_value(&mut self, depth: usize) -> Result<Value> {
        self.expect_simple(TokenKind::LeftBracket)?;
        let mut values = Vec::new();
        if self.eat_simple(TokenKind::RightBracket) {
            return Ok(Value::Array(values));
        }
        loop {
            values.push(self.value(depth)?);
            if values.len() >= 16_384 {
                return Err(Error::Limit("SQL array contains too many values".into()));
            }
            if self.eat_simple(TokenKind::RightBracket) {
                break;
            }
            self.expect_simple(TokenKind::Comma)?;
        }
        Ok(Value::Array(values))
    }

    fn field_path(&mut self) -> Result<FieldPath> {
        let path = self.identifier()?;
        FieldPath::parse(&path).map_err(|error| self.error(&error.to_string()))
    }

    fn identifier(&mut self) -> Result<String> {
        match self.current().kind.clone() {
            TokenKind::Word(value) => {
                self.cursor += 1;
                Ok(value)
            }
            _ => Err(self.error("expected an identifier")),
        }
    }

    fn expect_keyword(&mut self, keyword: &str) -> Result<()> {
        if self.eat_keyword(keyword) {
            Ok(())
        } else {
            Err(self.error(&format!("expected {keyword}")))
        }
    }

    fn expect_operator(&mut self, operator: &str) -> Result<()> {
        if self.current().kind == TokenKind::Operator(operator.to_owned()) {
            self.cursor += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected {operator}")))
        }
    }

    fn expect_simple(&mut self, kind: TokenKind) -> Result<()> {
        if self.eat_simple(kind.clone()) {
            Ok(())
        } else {
            Err(self.error("unexpected token"))
        }
    }

    fn eat_simple(&mut self, kind: TokenKind) -> bool {
        if self.current().kind == kind {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, keyword: &str) -> bool {
        match &self.current().kind {
            TokenKind::Word(word) if word.eq_ignore_ascii_case(keyword) => {
                self.cursor += 1;
                true
            }
            _ => false,
        }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.cursor]
    }

    fn error(&self, message: &str) -> Error {
        self.error_at(self.current().offset, message)
    }

    fn error_at(&self, offset: usize, message: &str) -> Error {
        Error::Query(format!("{message} at byte {offset}"))
    }
}

fn lex(source: &str) -> Result<Vec<Token>> {
    let chars: Vec<_> = source.char_indices().collect();
    let mut cursor = 0_usize;
    let mut tokens = Vec::new();
    while cursor < chars.len() {
        let (offset, ch) = chars[cursor];
        if ch.is_whitespace() {
            cursor += 1;
            continue;
        }
        let kind = if ch.is_ascii_alphabetic() || ch == '_' {
            let start = cursor;
            cursor += 1;
            while cursor < chars.len() {
                let next = chars[cursor].1;
                if next.is_ascii_alphanumeric() || next == '_' || next == '.' {
                    cursor += 1;
                } else {
                    break;
                }
            }
            let end = if cursor < chars.len() {
                chars[cursor].0
            } else {
                source.len()
            };
            TokenKind::Word(source[chars[start].0..end].to_owned())
        } else if ch.is_ascii_digit()
            || (ch == '-'
                && chars
                    .get(cursor + 1)
                    .is_some_and(|(_, next)| next.is_ascii_digit()))
        {
            let start = cursor;
            cursor += 1;
            while cursor < chars.len() && chars[cursor].1.is_ascii_digit() {
                cursor += 1;
            }
            if cursor < chars.len() && chars[cursor].1 == '.' {
                cursor += 1;
                while cursor < chars.len() && chars[cursor].1.is_ascii_digit() {
                    cursor += 1;
                }
            }
            if cursor < chars.len() && matches!(chars[cursor].1, 'e' | 'E') {
                cursor += 1;
                if cursor < chars.len() && matches!(chars[cursor].1, '+' | '-') {
                    cursor += 1;
                }
                let exponent_start = cursor;
                while cursor < chars.len() && chars[cursor].1.is_ascii_digit() {
                    cursor += 1;
                }
                if cursor == exponent_start {
                    return Err(Error::Query(format!("invalid exponent at byte {offset}")));
                }
            }
            let end = if cursor < chars.len() {
                chars[cursor].0
            } else {
                source.len()
            };
            TokenKind::Number(source[chars[start].0..end].to_owned())
        } else if ch == '\'' || ch == '"' {
            cursor += 1;
            let mut value = String::new();
            let mut closed = false;
            while cursor < chars.len() {
                let next = chars[cursor].1;
                if next == ch {
                    if chars.get(cursor + 1).is_some_and(|(_, after)| *after == ch) {
                        value.push(ch);
                        cursor += 2;
                        continue;
                    }
                    cursor += 1;
                    closed = true;
                    break;
                }
                value.push(next);
                cursor += 1;
                if value.len() > MAX_SQL_STRING_BYTES {
                    return Err(Error::Limit(format!(
                        "SQL string exceeds {MAX_SQL_STRING_BYTES} bytes"
                    )));
                }
            }
            if !closed {
                return Err(Error::Query(format!(
                    "unterminated string at byte {offset}"
                )));
            }
            TokenKind::String(value)
        } else {
            cursor += 1;
            match ch {
                '?' => TokenKind::Question,
                '*' => TokenKind::Star,
                ',' => TokenKind::Comma,
                '.' => TokenKind::Dot,
                '(' => TokenKind::LeftParen,
                ')' => TokenKind::RightParen,
                '{' => TokenKind::LeftBrace,
                '}' => TokenKind::RightBrace,
                '[' => TokenKind::LeftBracket,
                ']' => TokenKind::RightBracket,
                ':' => TokenKind::Colon,
                ';' => TokenKind::Semicolon,
                '=' | '<' | '>' | '!' => {
                    let next = chars.get(cursor).map(|(_, ch)| *ch);
                    let operator = match (ch, next) {
                        ('<', Some('=')) | ('>', Some('=')) | ('!', Some('=')) => {
                            cursor += 1;
                            format!("{ch}=")
                        }
                        ('<', Some('>')) => {
                            cursor += 1;
                            "<>".to_owned()
                        }
                        ('!', _) => {
                            return Err(Error::Query(format!("invalid operator at byte {offset}")));
                        }
                        _ => ch.to_string(),
                    };
                    TokenKind::Operator(operator)
                }
                _ => {
                    return Err(Error::Query(format!(
                        "unexpected character at byte {offset}"
                    )));
                }
            }
        };
        tokens.push(Token { kind, offset });
        if tokens.len() > MAX_SQL_TOKENS {
            return Err(Error::Limit(format!(
                "SQL statement exceeds {MAX_SQL_TOKENS} tokens"
            )));
        }
    }
    tokens.push(Token {
        kind: TokenKind::End,
        offset: source.len(),
    });
    Ok(tokens)
}
