use crate::blocker::{Blocker, BlockerError, BlockerOptions, BlockerResult};
use crate::cosmetic_filter_cache::{CosmeticFilterCache, UrlSpecificResources};
use crate::lists::{FilterSet, ParseOptions};
use crate::request::Request;
use crate::resources::{Resource, RedirectResource};

use std::collections::HashSet;

/// Main adblocking engine that allows efficient querying of resources to block.
pub struct Engine<'a> {
    pub blocker: Blocker<'a>,
    cosmetic_cache: CosmeticFilterCache,
}

impl<'a> Default for Engine<'a> {
    /// Equivalent to `Engine::new(true)`.
    fn default() -> Self {
        Self::new(true)
    }
}

impl<'a> Engine<'a> {
    /// Creates a new adblocking `Engine`. `Engine`s created without rules should generally only be
    /// used with deserialization.
    /// - `optimize` specifies whether or not to attempt to compress the internal representation by
    /// combining similar rules.
    pub fn new(optimize: bool) -> Self {
        let blocker_options = BlockerOptions {
            enable_optimizations: optimize,
        };

        Self {
            blocker: Blocker::new(vec![], blocker_options),
            cosmetic_cache: CosmeticFilterCache::new(),
        }
    }

    /// Loads rules in a single format, enabling optimizations and discarding debug information.
    pub fn from_rules(rules: &[String], opts: ParseOptions) -> Self {
        let mut filter_set = FilterSet::new(false);
        filter_set.add_filters(rules, opts);
        Self::from_filter_set(filter_set, true)
    }

    /// Loads rules, enabling optimizations and including debug information.
    pub fn from_rules_debug(rules: &[String], opts: ParseOptions) -> Self {
        Self::from_rules_parametrised(&rules, opts, true, true)
    }

    pub fn from_rules_parametrised(filter_rules: &[String], opts: ParseOptions, debug: bool, optimize: bool) -> Self {
        let mut filter_set = FilterSet::new(debug);
        filter_set.add_filters(filter_rules, opts);
        Self::from_filter_set(filter_set, optimize)
    }

    /// Loads rules from the given `FilterSet`. It is recommended to use a `FilterSet` when adding
    /// rules from multiple sources.
    pub fn from_filter_set(set: FilterSet, optimize: bool) -> Self {
        let FilterSet { network_filters, cosmetic_filters, .. } = set;

        let blocker_options = BlockerOptions {
            enable_optimizations: optimize,
        };

        Self {
            blocker: Blocker::new(network_filters, blocker_options),
            cosmetic_cache: CosmeticFilterCache::from_rules(cosmetic_filters),
        }
    }

    /// Serializes the `Engine` into a binary format so that it can be quickly reloaded later.
    pub fn serialize_raw(&'a self) -> Result<Vec<u8>, BlockerError> {
        use crate::data_format::SerializeFormat;

        let serialize_format = SerializeFormat::build(&self.blocker, &self.cosmetic_cache, false);

        serialize_format.serialize().map_err(|_e| {
            BlockerError::SerializationError
        })
    }

    /// Serializes the `Engine` into a compressed binary format so that it can be quickly reloaded later.
    ///
    /// The data format generated from this method is _not_ just a gzip compressed version of
    /// `serialize_raw`; it is a distinct format. If you'd like to convert data between the two
    /// formats, deserialize it into the `Engine` first, then serialize the appropriate type.
    ///
    /// This method will be removed in a future release. Going forwards, if you'd like to use a
    /// compressed binary format, use `serialize_raw` and bring your own compression/decompression.
    pub fn serialize_compressed(&'a self) -> Result<Vec<u8>, BlockerError> {
        use crate::data_format::SerializeFormat;

        let serialize_format = SerializeFormat::build(&self.blocker, &self.cosmetic_cache, true);

        serialize_format.serialize().map_err(|_e| {
            BlockerError::SerializationError
        })
    }

    /// Deserialize the `Engine` from the binary format generated by `Engine::serialize_compressed`
    /// or `Engine::serialize_raw`. The method will automatically select the correct
    /// deserialization implementation.
    pub fn deserialize(&mut self, serialized: &[u8]) -> Result<(), BlockerError> {
        use crate::data_format::DeserializeFormat;
        let current_tags = self.blocker.tags_enabled();
        let deserialize_format = DeserializeFormat::deserialize(serialized).map_err(|_e| {
            BlockerError::DeserializationError
        })?;
        let (blocker, cosmetic_cache) = deserialize_format.build();
        self.blocker = blocker;
        self.blocker.use_tags(&current_tags.iter().map(|s| &**s).collect::<Vec<_>>());
        self.cosmetic_cache = cosmetic_cache;
        Ok(())
    }

    /// Check if a request for a network resource from `url`, of type `request_type`, initiated by
    /// `source_url`, should be blocked.
    pub fn check_network_urls(&self, url: &str, source_url: &str, request_type: &str) -> BlockerResult {
        Request::from_urls(&url, &source_url, &request_type)
        .map(|request| {
            self.blocker.check(&request)
        })
        .unwrap_or_else(|_e| {
            BlockerResult {
                matched: false,
                important: false,
                redirect: None,
                exception: None,
                filter: None,
                error: Some("Error parsing request".to_owned())
            }
        })
    }

    pub fn check_network_urls_with_hostnames(
        &self,
        url: &str,
        hostname: &str,
        source_hostname: &str,
        request_type: &str,
        third_party_request: Option<bool>
    ) -> BlockerResult {
        let request = Request::from_urls_with_hostname(url, hostname, source_hostname, request_type, third_party_request);
        self.blocker.check(&request)
    }

    pub fn check_network_urls_with_hostnames_subset(
        &self,
        url: &str,
        hostname: &str,
        source_hostname: &str,
        request_type: &str,
        third_party_request: Option<bool>,
        previously_matched_rule: bool,
        force_check_exceptions: bool,
    ) -> BlockerResult {
        let request = Request::from_urls_with_hostname(url, hostname, source_hostname, request_type, third_party_request);
        self.blocker.check_parameterised(&request, previously_matched_rule, force_check_exceptions)
    }

    /// Returns a string containing any additional CSP directives that should be added to this
    /// request's response. Only applies to document and subdocument requests.
    ///
    /// If multiple policies are present from different rules, they will be joined by commas.
    pub fn get_csp_directives(
        &self,
        url: &str,
        hostname: &str,
        source_hostname: &str,
        request_type: &str,
        third_party_request: Option<bool>,
    ) -> Option<String> {
        let request = Request::from_urls_with_hostname(url, hostname, source_hostname, request_type, third_party_request);
        self.blocker.get_csp_directives(&request)
    }

    /// Check if a given filter has been previously added to this `Engine`.
    ///
    /// Note that only network filters are currently supported by this method.
    pub fn filter_exists(&self, filter: &str) -> bool {
        use crate::filters::network::NetworkFilter;
        let filter_parsed = NetworkFilter::parse(filter, false, Default::default());
        match filter_parsed.map(|f| self.blocker.filter_exists(&f)) {
            Ok(exists) => exists,
            Err(_e) => {
                #[cfg(test)]
                eprintln!("Encountered unparseable filter when checking for filter existence: {:?}", _e);
                false
            }
        }
    }

    /// Sets this engine's tags to be _only_ the ones provided in `tags`.
    ///
    /// Tags can be used to cheaply enable or disable network rules with a corresponding `$tag`
    /// option.
    pub fn use_tags(&mut self, tags: &[&str]) {
        self.blocker.use_tags(tags);
    }

    /// Sets this engine's tags to additionally include the ones provided in `tags`.
    ///
    /// Tags can be used to cheaply enable or disable network rules with a corresponding `$tag`
    /// option.
    pub fn enable_tags(&mut self, tags: &[&str]) {
        self.blocker.enable_tags(tags);
    }

    /// Sets this engine's tags to no longer include the ones provided in `tags`.
    ///
    /// Tags can be used to cheaply enable or disable network rules with a corresponding `$tag`
    /// option.
    pub fn disable_tags(&mut self, tags: &[&str]) {
        self.blocker.disable_tags(tags);
    }

    /// Checks if a given tag exists in this engine.
    ///
    /// Tags can be used to cheaply enable or disable network rules with a corresponding `$tag`
    /// option.
    pub fn tag_exists(&self, tag: &str) -> bool {
        self.blocker.tags_enabled().contains(&tag.to_owned())
    }

    /// Sets this engine's resources to be _only_ the ones provided in `resources`.
    pub fn use_resources(&mut self, resources: &[Resource]) {
        self.blocker.use_resources(resources);
        self.cosmetic_cache.use_resources(resources);
    }

    /// Sets this engine's resources to additionally include `resource`.
    pub fn add_resource(&mut self, resource: Resource) -> Result<(), crate::resources::AddResourceError> {
        self.blocker.add_resource(&resource)?;
        self.cosmetic_cache.add_resource(&resource)?;
        Ok(())
    }

    /// Gets a previously added resource from the engine.
    pub fn get_resource(&self, key: &str) -> Option<RedirectResource> {
        self.blocker.get_resource(key).cloned()
    }

    // Cosmetic filter functionality

    /// If any of the provided CSS classes or ids could cause a certain generic CSS hide rule
    /// (i.e. `{ display: none !important; }`) to be required, this method will return a list of
    /// CSS selectors corresponding to rules referencing those classes or ids, provided that the
    /// corresponding rules are not excepted.
    ///
    /// `exceptions` should be passed directly from `UrlSpecificResources`.
    pub fn hidden_class_id_selectors(&self, classes: &[String], ids: &[String], exceptions: &HashSet<String>) -> Vec<String> {
        self.cosmetic_cache.hidden_class_id_selectors(classes, ids, exceptions)
    }

    /// Returns a set of cosmetic filter resources required for a particular url. Once this has
    /// been called, all CSS ids and classes on a page should be passed to
    /// `hidden_class_id_selectors` to obtain any stylesheets consisting of generic rules (if the
    /// returned `generichide` value is false).
    pub fn url_cosmetic_resources(&self, url: &str) -> UrlSpecificResources {
        let request = Request::from_url(url);
        if request.is_err() {
            return UrlSpecificResources::empty();
        }
        let request = request.unwrap();

        let generichide = self.blocker.check_generic_hide(&request);
        self.cosmetic_cache.hostname_cosmetic_resources(&request.hostname, generichide)
    }
}

/// Static assertions for `Engine: Send + Sync` traits.
#[cfg(not(any(feature = "object-pooling", feature = "optimized-regex-cache")))]
fn _assertions() {
    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}

    _assert_send::<Engine>();
    _assert_sync::<Engine>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ResourceType, MimeType};
    use crate::blocker::Redirection;
    use crate::lists::FilterFormat;

    #[test]
    fn tags_enable_adds_tags() {
        let filters = vec![
            String::from("adv$tag=stuff"),
            String::from("somelongpath/test$tag=stuff"),
            String::from("||brianbondy.com/$tag=brian"),
            String::from("||brave.com$tag=brian"),
        ];
        let url_results = vec![
            ("http://example.com/advert.html", true),
            ("http://example.com/somelongpath/test/2.html", true),
            ("https://brianbondy.com/about", true),
            ("https://brave.com/about", true),
        ];

        let mut engine = Engine::from_rules(&filters, Default::default());
        engine.enable_tags(&["stuff"]);
        engine.enable_tags(&["brian"]);

        url_results.into_iter().for_each(|(url, expected_result)| {
            let matched_rule = engine.check_network_urls(&url, "", "");
            if expected_result {
                assert!(matched_rule.matched, "Expected match for {}", url);
            } else {
                assert!(!matched_rule.matched, "Expected no match for {}, matched with {:?}", url, matched_rule.filter);
            }
        });
    }

    #[test]
    fn tags_disable_works() {
        let filters = vec![
            String::from("adv$tag=stuff"),
            String::from("somelongpath/test$tag=stuff"),
            String::from("||brianbondy.com/$tag=brian"),
            String::from("||brave.com$tag=brian"),
        ];
        let url_results = vec![
            ("http://example.com/advert.html", false),
            ("http://example.com/somelongpath/test/2.html", false),
            ("https://brianbondy.com/about", true),
            ("https://brave.com/about", true),
        ];

        let mut engine = Engine::from_rules(&filters, Default::default());
        engine.enable_tags(&["brian", "stuff"]);
        engine.disable_tags(&["stuff"]);

        url_results.into_iter().for_each(|(url, expected_result)| {
            let matched_rule = engine.check_network_urls(&url, "", "");
            if expected_result {
                assert!(matched_rule.matched, "Expected match for {}", url);
            } else {
                assert!(!matched_rule.matched, "Expected no match for {}, matched with {:?}", url, matched_rule.filter);
            }
        });
    }

    #[test]
    fn exception_tags_inactive_by_default() {
        let filters = vec![
            String::from("adv"),
            String::from("||brianbondy.com/$tag=brian"),
            String::from("@@||brianbondy.com/$tag=brian"),
        ];
        let url_results = vec![
            ("http://example.com/advert.html", true),
            ("https://brianbondy.com/about", false),
            ("https://brianbondy.com/advert", true),
        ];

        let engine = Engine::from_rules(&filters, Default::default());

        url_results.into_iter().for_each(|(url, expected_result)| {
            let matched_rule = engine.check_network_urls(&url, "", "");
            if expected_result {
                assert!(matched_rule.matched, "Expected match for {}", url);
            } else {
                assert!(!matched_rule.matched, "Expected no match for {}, matched with {:?}", url, matched_rule.filter);
            }
        });
    }

    #[test]
    fn exception_tags_works() {
        let filters = vec![
            String::from("adv"),
            String::from("||brianbondy.com/$tag=brian"),
            String::from("@@||brianbondy.com/$tag=brian"),
        ];
        let url_results = vec![
            ("http://example.com/advert.html", true),
            ("https://brianbondy.com/about", false),
            ("https://brianbondy.com/advert", false),
        ];

        let mut engine = Engine::from_rules(&filters, Default::default());
        engine.enable_tags(&["brian", "stuff"]);

        url_results.into_iter().for_each(|(url, expected_result)| {
            let matched_rule = engine.check_network_urls(&url, "", "");
            if expected_result {
                assert!(matched_rule.matched, "Expected match for {}", url);
            } else {
                assert!(!matched_rule.matched, "Expected no match for {}, matched with {:?}", url, matched_rule.filter);
            }
        });
    }

    #[test]
    #[ignore] // TODO
    fn serialization_retains_tags() {
        let filters = vec![
            String::from("adv$tag=stuff"),
            String::from("somelongpath/test$tag=stuff"),
            String::from("||brianbondy.com/$tag=brian"),
            String::from("||brave.com$tag=brian"),
        ];
        let url_results = vec![
            ("http://example.com/advert.html", true),
            ("http://example.com/somelongpath/test/2.html", true),
            ("https://brianbondy.com/about", false),
            ("https://brave.com/about", false),
        ];

        let mut engine = Engine::from_rules(&filters, Default::default());
        engine.enable_tags(&["stuff"]);
        engine.enable_tags(&["brian"]);
        let serialized = engine.serialize_raw().unwrap();
        let mut deserialized_engine = Engine::default();
        deserialized_engine.enable_tags(&["stuff"]);
        deserialized_engine.deserialize(&serialized).unwrap();

        url_results.into_iter().for_each(|(url, expected_result)| {
            let matched_rule = deserialized_engine.check_network_urls(&url, "", "");
            if expected_result {
                assert!(matched_rule.matched, "Expected match for {}", url);
            } else {
                assert!(!matched_rule.matched, "Expected no match for {}, matched with {:?}", url, matched_rule.filter);
            }
        });
    }

    #[test]
    fn deserialization_backwards_compatible_plain() {
        // deserialization_generate_simple();
        // assert!(false);
        let serialized: Vec<u8> = vec![31, 139, 8, 0, 0, 0, 0, 0, 0, 255, 1, 68, 0, 187, 255, 155, 145, 128, 145, 128,
            145, 128, 145, 128, 145, 128, 145, 129, 207, 202, 167, 36, 217, 43, 56, 97, 176, 145, 158, 145, 206, 0, 3,
            31, 255, 146, 1, 145, 169, 97, 100, 45, 98, 97, 110, 110, 101, 114, 192, 192, 192, 192, 192, 192, 192, 192,
            207, 186, 136, 69, 13, 115, 187, 170, 226, 192, 192, 192, 144, 194, 195, 194, 195, 207, 77, 26, 78, 68, 0,
            0, 0];

        let mut deserialized_engine = Engine::default();
        deserialized_engine.deserialize(&serialized).unwrap();

        let url = "http://example.com/ad-banner.gif";
        let matched_rule = deserialized_engine.check_network_urls(url, "", "");
        assert!(matched_rule.matched, "Expected match for {}", url);
    }

    #[test]
    fn deserialization_backwards_compatible_tags() {
        // deserialization_generate_tags();
        // assert!(false);
        let serialized: Vec<u8> = vec![31, 139, 8, 0, 0, 0, 0, 0, 0, 255, 149, 139, 49, 14, 64, 48, 24, 70, 137, 131, 88,
            108, 98, 148, 184, 135, 19, 252, 197, 218, 132, 3, 8, 139, 85, 126, 171, 132, 193, 32, 54, 71, 104, 218, 205,
            160, 139, 197, 105, 218, 166, 233, 5, 250, 125, 219, 203, 123, 43, 14, 238, 163, 124, 206, 228, 79, 11, 184,
            113, 195, 55, 136, 98, 181, 132, 120, 65, 157, 17, 160, 180, 233, 152, 221, 1, 164, 98, 178, 255, 242, 178,
            221, 231, 201, 0, 19, 122, 216, 92, 112, 161, 1, 58, 213, 199, 143, 114, 0, 0, 0];
        let mut deserialized_engine = Engine::default();

        deserialized_engine.enable_tags(&[]);
        deserialized_engine.deserialize(&serialized).unwrap();
        let url = "http://example.com/ad-banner.gif";
        let matched_rule = deserialized_engine.check_network_urls(url, "", "");
        assert!(!matched_rule.matched, "Expected NO match for {}", url);

        deserialized_engine.enable_tags(&["abc"]);
        deserialized_engine.deserialize(&serialized).unwrap();

        let url = "http://example.com/ad-banner.gif";
        let matched_rule = deserialized_engine.check_network_urls(url, "", "");
        assert!(matched_rule.matched, "Expected match for {}", url);
    }

    #[test]
    fn deserialization_backwards_compatible_resources() {
        // deserialization_generate_resources();
        // assert!(false);
        let serialized: Vec<u8> = vec![31, 139, 8, 0, 0, 0, 0, 0, 0, 255, 61, 139, 189, 10, 64, 80, 28, 197, 201, 46,
            229, 1, 44, 54, 201, 234, 117, 174, 143, 65, 233, 18, 6, 35, 118, 229, 127, 103, 201, 230, 99, 146, 39,
            184, 177, 25, 152, 61, 13, 238, 29, 156, 83, 167, 211, 175, 115, 90, 40, 184, 203, 235, 24, 244, 219, 176,
            209, 2, 29, 156, 130, 164, 61, 68, 132, 9, 121, 166, 131, 48, 246, 19, 74, 71, 28, 69, 113, 230, 231, 25,
            101, 186, 42, 121, 86, 73, 189, 42, 95, 103, 255, 102, 219, 183, 29, 170, 127, 68, 102, 150, 86, 28, 162,
            0, 247, 3, 163, 110, 154, 146, 145, 195, 175, 245, 47, 101, 250, 113, 201, 119, 0, 0, 0];

        let mut deserialized_engine = Engine::default();
        deserialized_engine.deserialize(&serialized).unwrap();

        let url = "http://example.com/ad-banner.gif";
        let matched_rule = deserialized_engine.check_network_urls(url, "", "");
        // This serialized DAT was generated prior to
        // https://github.com/brave/adblock-rust/pull/185, so the `redirect` filter did not get
        // duplicated into the list of blocking filters.
        //
        // TODO - The failure to match here is considered acceptable for now, as it's part of a
        // breaking change (minor version bump). However, the test should be updated at some point.
        //assert!(matched_rule.matched, "Expected match for {}", url);
        assert_eq!(matched_rule.redirect, Some(Redirection::Resource("data:text/plain;base64,".to_owned())), "Expected redirect to contain resource");
    }

    #[test]
    #[ignore] // TODO
    fn deserialization_generate_simple() {
        let mut engine = Engine::from_rules(&[
            "ad-banner".to_owned()
        ], Default::default());
        let serialized = engine.serialize_compressed().unwrap();
        println!("Engine serialized: {:?}", serialized);
        engine.deserialize(&serialized).unwrap();
    }

    #[test]
    #[ignore] // TODO
    fn deserialization_generate_tags() {
        let mut engine = Engine::from_rules(&[
            "ad-banner$tag=abc".to_owned()
        ], Default::default());
        engine.use_tags(&["abc"]);
        let serialized = engine.serialize_compressed().unwrap();
        println!("Engine serialized: {:?}", serialized);
        engine.deserialize(&serialized).unwrap();
    }

    #[test]
    #[ignore] // TODO
    fn deserialization_generate_resources() {
        let mut engine = Engine::from_rules(&[
            "ad-banner$redirect=nooptext".to_owned()
        ], Default::default());

        let resources = vec![
            Resource {
                name: "nooptext".to_string(),
                aliases: vec![],
                kind: ResourceType::Mime(MimeType::TextPlain),
                content: base64::encode(""),
            },
            Resource {
                name: "noopcss".to_string(),
                aliases: vec![],
                kind: ResourceType::Mime(MimeType::TextPlain),
                content: base64::encode(""),
            },
        ];
        engine.use_resources(&resources);

        let serialized = engine.serialize_compressed().unwrap();
        println!("Engine serialized: {:?}", serialized);
        engine.deserialize(&serialized).unwrap();
    }

    #[test]
    fn redirect_resource_insertion_works() {
        let mut engine = Engine::from_rules(&[
            "ad-banner$redirect=nooptext".to_owned()
        ], Default::default());

        engine.add_resource(Resource {
            name: "nooptext".to_owned(),
            aliases: vec![],
            kind: ResourceType::Mime(MimeType::TextPlain),
            content: "".to_owned(),
        }).unwrap();

        let url = "http://example.com/ad-banner.gif";
        let matched_rule = engine.check_network_urls(url, "", "");
        assert!(matched_rule.matched, "Expected match for {}", url);
        assert_eq!(matched_rule.redirect, Some(Redirection::Resource("data:text/plain;base64,".to_owned())), "Expected redirect to contain resource");
    }

    #[test]
    fn redirect_resource_lookup_works() {
        let script = base64::encode(r#"
(function() {
	;
})();

        "#);

        let mut engine = Engine::default();

        engine.add_resource(Resource {
            name: "noopjs".to_owned(),
            aliases: vec![],
            kind: ResourceType::Mime(MimeType::ApplicationJavascript),
            content: script.to_owned(),
        }).unwrap();
        let inserted_resource = engine.get_resource("noopjs");
        assert!(inserted_resource.is_some());
        let resource = inserted_resource.unwrap();
        assert_eq!(resource.content_type, "application/javascript");
        assert_eq!(resource.data, script);
    }

    #[test]
    fn document() {
        let filters = vec![
            String::from("||example.com$document"),
            String::from("@@||sub.example.com$document"),
        ];

        let engine = Engine::from_rules_debug(&filters, Default::default());

        assert!(engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        assert!(!engine.check_network_urls("https://example.com", "https://example.com", "script").matched);
        assert!(engine.check_network_urls("https://sub.example.com", "https://sub.example.com", "document").exception.is_some());
    }

    #[test]
    fn implicit_all() {
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com^")], Default::default());
            assert!(engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com^$first-party,match-case")], Default::default());
            assert!(engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com^$script")], Default::default());
            assert!(!engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com^$~script")], Default::default());
            assert!(!engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com^$document"), String::from("@@||example.com^$generichide")], Default::default());
            assert!(engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("example.com")], ParseOptions { format: FilterFormat::Hosts, ..Default::default() });
            assert!(engine.check_network_urls("https://example.com", "https://example.com", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com/path")], Default::default());
            assert!(!engine.check_network_urls("https://example.com/path", "https://example.com/path", "document").matched);
        }
        {
            let engine = Engine::from_rules_debug(&vec![String::from("||example.com/path^")], Default::default());
            assert!(!engine.check_network_urls("https://example.com/path", "https://example.com/path", "document").matched);
        }
    }

    #[test]
    fn generichide() {
        let filters = vec![
            String::from("##.donotblock"),
            String::from("##a[href=\"generic.com\"]"),

            String::from("@@||example.com$generichide"),
            String::from("example.com##.block"),

            String::from("@@||example2.com/test.html$generichide"),
            String::from("example2.com##.block"),
        ];
        let url_results = vec![
            ("https://example.com", vec![".block"], true),
            ("https://example.com/test.html", vec![".block"], true),
            ("https://example2.com", vec![".block", "a[href=\"generic.com\"]"], false),
            ("https://example2.com/test.html", vec![".block"], true),
        ];

        let engine = Engine::from_rules(&filters, Default::default());

        url_results.into_iter().for_each(|(url, expected_result, expected_generichide)| {
            let result = engine.url_cosmetic_resources(url);
            assert_eq!(result.hide_selectors, expected_result.iter().map(|s| s.to_string()).collect::<HashSet<_>>());
            assert_eq!(result.generichide, expected_generichide);
        });
    }

    #[test]
    fn important_redirect() {
        let mut filter_set = FilterSet::new(true);
        filter_set.add_filters(&vec![
            "||addthis.com^$important,3p,domain=~missingkids.com|~missingkids.org|~sainsburys.jobs|~sitecore.com|~amd.com".to_string(),
            "||addthis.com/*/addthis_widget.js$script,redirect=addthis.com/addthis_widget.js".to_string(),
        ], Default::default());
        let mut engine = Engine::from_filter_set(filter_set, false);

        engine.add_resource(Resource {
            name: "addthis.com/addthis_widget.js".to_owned(),
            aliases: vec![],
            kind: ResourceType::Mime(MimeType::ApplicationJavascript),
            content: base64::encode("window.addthis = undefined"),
        }).unwrap();

        let result = engine.check_network_urls("https://s7.addthis.com/js/250/addthis_widget.js?pub=resto", "https://www.rhmodern.com/catalog/product/product.jsp?productId=prod14970086&categoryId=cat7150028", "script");

        assert!(result.redirect.is_some());
    }
}
