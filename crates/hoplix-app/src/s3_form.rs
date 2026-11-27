use hoplix_core::{S3Options, is_valid_bucket};

use crate::connect_form::FormError;

// Types

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Field {
    Bucket(String),
    Region(String),
    PathStyle(bool),
    PlainHttp(bool),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct S3Form {
    pub bucket: String,
    pub region: String,
    pub path_style: bool,
    pub plain_http: bool,
}

// Editing

impl S3Form {
    pub fn fill(options: &S3Options) -> Self {
        Self {
            bucket: options.bucket.clone(),
            region: options.region.clone(),
            path_style: options.path_style,
            plain_http: options.plain_http,
        }
    }

    pub fn apply(&mut self, field: S3Field) {
        match field {
            S3Field::Bucket(bucket) => self.bucket = bucket,
            S3Field::Region(region) => self.region = region,
            S3Field::PathStyle(enabled) => self.path_style = enabled,
            S3Field::PlainHttp(enabled) => self.plain_http = enabled,
        }
    }

    pub fn options(&self) -> S3Options {
        S3Options {
            bucket: self.bucket.trim().to_string(),
            region: self.region.trim().to_string(),
            path_style: self.path_style,
            plain_http: self.plain_http,
            session_token: None,
        }
    }

    pub fn validate(&self) -> Result<S3Options, FormError> {
        let options = self.options();
        if options.bucket.is_empty() {
            return Err(FormError::MissingBucket);
        }
        if !is_valid_bucket(&options.bucket) {
            return Err(FormError::InvalidBucket);
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_fill_and_validate() {
        let mut form = S3Form::default();
        assert_eq!(form.validate(), Err(FormError::MissingBucket));
        form.apply(S3Field::Bucket("a b".to_string()));
        assert_eq!(form.validate(), Err(FormError::InvalidBucket));
        form.apply(S3Field::Bucket(" demo-bucket ".to_string()));
        form.apply(S3Field::Region(" eu-central-1 ".to_string()));
        form.apply(S3Field::PathStyle(true));
        form.apply(S3Field::PlainHttp(true));
        let options = form.validate().expect("valid");
        assert_eq!(options.bucket, "demo-bucket");
        assert_eq!(options.region, "eu-central-1");
        assert!(options.path_style && options.plain_http);
        assert_eq!(
            S3Form::fill(&options),
            S3Form {
                bucket: "demo-bucket".to_string(),
                region: "eu-central-1".to_string(),
                ..form
            }
        );
    }
}
