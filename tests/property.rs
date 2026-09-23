use proptest::prelude::*;
use std::path::PathBuf;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn property_load_invalid_path_fails(path_str in "[a-z]{1,10}\\.jpg") {
        let path = PathBuf::from(&path_str);
        let result = image_viewer::load_image(&path, 1600);
        prop_assert!(result.is_err());
    }
}
