use crate::util::feeppm_effective;

#[test]
fn test_effective_feeppm() {
    assert_eq!(feeppm_effective(0, 0, 1_000), 0);
    assert_eq!(feeppm_effective(0, 0, 200_000), 0);
    assert_eq!(feeppm_effective(0, 0, 9_999_999_999), 0);

    assert_eq!(feeppm_effective(1, 0, 1_000), 1);
    assert_eq!(feeppm_effective(68, 0, 1_000), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 1_000), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 1_000), 1_000);
    assert_eq!(feeppm_effective(0, 53, 1_000), 53_000);
    assert_eq!(feeppm_effective(0, 10_009_000, 1_000), 10_009_000_000);

    assert_eq!(feeppm_effective(1, 0, 200_000), 1);
    assert_eq!(feeppm_effective(68, 0, 200_000), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 200_000), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 200_000), 5);
    assert_eq!(feeppm_effective(0, 53, 200_000), 265);
    assert_eq!(feeppm_effective(0, 10_009_000, 200_000), 50_045_000);

    assert_eq!(feeppm_effective(100, 0, 9_999_999_999), 100);
    assert_eq!(feeppm_effective(68, 0, 9_999_999_999), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 9_999_999_999), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 9_999_999_999), 1);
    assert_eq!(feeppm_effective(0, 53, 9_999_999_999), 1);
    assert_eq!(feeppm_effective(0, 10_009_000, 9_999_999_999), 1_001);

    assert_eq!(feeppm_effective(1, 1, 1_000), 1_001);
    assert_eq!(feeppm_effective(68, 53, 1_000), 53_068);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 1_000),
        10_020_115_555
    );

    assert_eq!(feeppm_effective(1, 1, 200_000), 6);
    assert_eq!(feeppm_effective(68, 53, 200_000), 333);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 200_000),
        61_160_555
    );

    assert_eq!(feeppm_effective(1, 1, 9_999_999_999), 2);
    assert_eq!(feeppm_effective(68, 53, 9_999_999_999), 69);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 9_999_999_999),
        11_116_556
    );

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 1_000),
        4_299_262_262_296
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 1_000), 4_294_967_295_000);
    assert_eq!(feeppm_effective(u32::MAX, 0, 1_000), 4_294_967_296);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 200_000),
        25_769_803_770
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 200_000), 21_474_836_475);
    assert_eq!(feeppm_effective(u32::MAX, 0, 200_000), 4_294_967_296);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 9_999_999_999),
        4_295_396_792
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 9_999_999_999), 429_497);
    assert_eq!(feeppm_effective(u32::MAX, 0, 9_999_999_999), 4_294_967_296);

    assert_eq!(feeppm_effective(0, 0, u64::MAX), 0);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, u64::MAX),
        4_294_967_296
    );
    assert_eq!(feeppm_effective(0, u32::MAX, u64::MAX), 1);
    assert_eq!(feeppm_effective(u32::MAX, 0, u64::MAX), 4_294_967_296);
}

#[test]
fn test_fee_total() {
    use crate::util::fee_total_msat_precise;
    assert_eq!(fee_total_msat_precise(0, 0, 1_000).ceil() as u64, 0);

    assert_eq!(fee_total_msat_precise(1, 0, 1_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(349, 0, 1_000).ceil() as u64, 1);
    assert_eq!(
        fee_total_msat_precise(u32::MAX, 0, 1_000).ceil() as u64,
        4_294_968
    );

    assert_eq!(fee_total_msat_precise(0, 1, 1_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(0, 1234, 1_000).ceil() as u64, 1234);
    assert_eq!(
        fee_total_msat_precise(0, u32::MAX, 1_000).ceil() as u64,
        4_294_967_295
    );

    assert_eq!(fee_total_msat_precise(1, 1, 1_000).ceil() as u64, 2);
    assert_eq!(
        fee_total_msat_precise(349, 1234, 1_000).ceil() as u64,
        1_235
    );
    assert_eq!(
        fee_total_msat_precise(u32::MAX, u32::MAX, 1_000).ceil() as u64,
        4_299_262_263
    );

    assert_eq!(fee_total_msat_precise(0, 0, 200_000).ceil() as u64, 0);

    assert_eq!(fee_total_msat_precise(1, 0, 200_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(349, 0, 200_000).ceil() as u64, 70);
    assert_eq!(
        fee_total_msat_precise(u32::MAX, 0, 200_000).ceil() as u64,
        858_993_460
    );

    assert_eq!(fee_total_msat_precise(0, 1, 200_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(0, 1234, 200_000).ceil() as u64, 1234);
    assert_eq!(
        fee_total_msat_precise(0, u32::MAX, 200_000).ceil() as u64,
        4_294_967_295
    );

    assert_eq!(fee_total_msat_precise(1, 1, 200_000).ceil() as u64, 2);
    assert_eq!(
        fee_total_msat_precise(349, 1234, 200_000).ceil() as u64,
        1_304
    );
    assert_eq!(
        fee_total_msat_precise(u32::MAX, u32::MAX, 200_000).ceil() as u64,
        5_153_960_754
    );
}

#[test]
fn test_feeppm_effective_from_amts() {
    use crate::util::feeppm_effective_from_amts;
    assert_eq!(feeppm_effective_from_amts(1_000, 1_000), 0);
    assert_eq!(feeppm_effective_from_amts(u64::MAX, u64::MAX), 0);

    assert_eq!(feeppm_effective_from_amts(200_000_200, 200_000_000), 1);
    assert_eq!(feeppm_effective_from_amts(201_001_234, 201_000_000), 7);
    assert_eq!(feeppm_effective_from_amts(100_125_200, 100_000_000), 1_252);

    let result1 = std::panic::catch_unwind(|| feeppm_effective_from_amts(1_000, 2_000));
    assert!(result1.is_err());
}
