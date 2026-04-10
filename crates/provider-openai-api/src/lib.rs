use aistatus_core::UsageFamily;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiApiUsageProvider;

impl OpenAiApiUsageProvider {
    pub fn usage_family(&self) -> UsageFamily {
        UsageFamily::Api
    }

    pub fn provider_name(&self) -> &'static str {
        "openai_api_usage"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_api_provider_reports_api_usage_family() {
        let provider = OpenAiApiUsageProvider;
        assert_eq!(provider.usage_family(), UsageFamily::Api);
        assert_eq!(provider.provider_name(), "openai_api_usage");
    }
}
