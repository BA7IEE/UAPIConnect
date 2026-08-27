//! Product-distribution policy for the U-API Connect edition.
//!
//! Keep white-label values here so upstream synchronization has a small,
//! auditable integration surface. Never place credentials in this module.

pub const PRODUCT_ID: &str = "uapi-connect";
pub const PRODUCT_NAME: &str = "U-API Connect";
pub const MANAGER_TITLE: &str = "U-API Connect";
pub const MANAGER_DISPLAY_NAME: &str = "U-API Connect 设置";
pub const PRODUCT_SUBTITLE: &str = "Codex 接入工具";
pub const PUBLISHER: &str = "U-Studio";
pub const SILENT_BUNDLE_ID: &str = "cn.u-studio.uapi.connect";
pub const MANAGER_BUNDLE_ID: &str = "cn.u-studio.uapi.connect.manager";
pub const URL_SCHEME: &str = "uapiconnect";

pub const FIXED_PROVIDER_ID: &str = "uapi_connect";
pub const FIXED_PROVIDER_NAME: &str = "U-API Connect";
pub const FIXED_BASE_URL: &str = "https://token.u-studio.cn/v1";

pub const FIXED_PROVIDER_EDITION: bool = true;
pub const ADS_ENABLED: bool = false;
pub const UPDATES_ENABLED: bool = false;
pub const BACKGROUND_FEATURES_ENABLED: bool = false;

pub const UPDATE_FEED_URL: &str = "https://token.u-studio.cn/uapi-connect/latest.json";
pub const UPDATE_REPOSITORY: &str = "BA7IEE/UAPIConnect";
pub const SOURCE_UPSTREAM_URL: &str = "https://github.com/BigPizzaV3/CodexPlusPlus";
pub const HELP_URL: &str = "https://token.u-studio.cn/";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn rust_constants_match_distribution_manifest() {
        let manifest: Value =
            serde_json::from_str(include_str!("../../../distribution/uapi-connect.json"))
                .expect("distribution manifest must be valid JSON");

        assert_eq!(manifest["productId"].as_str(), Some(PRODUCT_ID));
        assert_eq!(manifest["productName"].as_str(), Some(PRODUCT_NAME));
        assert_eq!(manifest["managerTitle"].as_str(), Some(MANAGER_TITLE));
        assert_eq!(
            manifest["managerDisplayName"].as_str(),
            Some(MANAGER_DISPLAY_NAME)
        );
        assert_eq!(manifest["productSubtitle"].as_str(), Some(PRODUCT_SUBTITLE));
        assert_eq!(manifest["publisher"].as_str(), Some(PUBLISHER));
        assert_eq!(manifest["silentBundleId"].as_str(), Some(SILENT_BUNDLE_ID));
        assert_eq!(
            manifest["managerBundleId"].as_str(),
            Some(MANAGER_BUNDLE_ID)
        );
        assert_eq!(manifest["urlScheme"].as_str(), Some(URL_SCHEME));
        assert_eq!(
            manifest["fixedProviderId"].as_str(),
            Some(FIXED_PROVIDER_ID)
        );
        assert_eq!(
            manifest["fixedProviderName"].as_str(),
            Some(FIXED_PROVIDER_NAME)
        );
        assert_eq!(manifest["fixedBaseUrl"].as_str(), Some(FIXED_BASE_URL));
        assert_eq!(manifest["helpUrl"].as_str(), Some(HELP_URL));
        assert_eq!(
            manifest["upstreamSourceUrl"].as_str(),
            Some(SOURCE_UPSTREAM_URL)
        );
        assert_eq!(manifest["updateFeedUrl"].as_str(), Some(UPDATE_FEED_URL));
        assert_eq!(
            manifest["updateRepository"].as_str(),
            Some(UPDATE_REPOSITORY)
        );
        assert_eq!(
            manifest["features"]["fixedProviderEdition"].as_bool(),
            Some(FIXED_PROVIDER_EDITION)
        );
        assert_eq!(
            manifest["features"]["adsEnabled"].as_bool(),
            Some(ADS_ENABLED)
        );
        assert_eq!(
            manifest["features"]["updatesEnabled"].as_bool(),
            Some(UPDATES_ENABLED)
        );
        assert_eq!(
            manifest["features"]["backgroundFeaturesEnabled"].as_bool(),
            Some(BACKGROUND_FEATURES_ENABLED)
        );
    }
}
