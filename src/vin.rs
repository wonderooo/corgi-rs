//! VIN primitives: structure validation, WMI extraction, check digit and model
//! year.
//!
//! These follow NHTSA's own `fVinWMI`, `fVinCheckDigit2` and `fVinModelYear2`
//! rather than the textbook rules, because the decoder's schema lookup has to
//! agree with them exactly.

/// Reasons why the VIN structure validation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureErrorCode {
    InvalidLength,
    InvalidCharacters,
}

/// A character that has no business being in a VIN, and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCharacter {
    pub character: char,
    /// 1-based VIN position, matching how NHTSA reports it.
    pub position: usize,
}

/// Characters that never appear in a VIN, because they read like digits.
const EXCLUDED: [char; 3] = ['I', 'O', 'Q'];

/// Whether `c` is legal anywhere in a VIN.
fn is_vin_char(c: char) -> bool {
    (c.is_ascii_digit() || c.is_ascii_uppercase()) && !EXCLUDED.contains(&c)
}

/// Check that the VIN is 17 characters drawn from the VIN alphabet.
///
/// Position 9 additionally has to be a digit or `X`. Position 10 is left to
/// [`model_year`], so that a VIN whose only fault is an unencoded model year is
/// reported as such rather than as a malformed VIN.
///
/// # Examples
///
/// ```
/// use corgi_rs::vin::validate_structure;
/// assert!(validate_structure("1C6RR7LT2JS179571").is_ok());
/// assert!(validate_structure("1C6RR7LT2JS17957").is_err());  // 16 characters
/// assert!(validate_structure("1C6RR7LT2JS17957I").is_err()); // I is excluded
/// ```
pub fn validate_structure(vin: &str) -> Result<(), (StructureErrorCode, Vec<InvalidCharacter>)> {
    if vin.chars().count() != 17 {
        return Err((StructureErrorCode::InvalidLength, Vec::new()));
    }

    let invalid: Vec<InvalidCharacter> = vin
        .chars()
        .enumerate()
        .filter_map(|(idx, c)| {
            let ok = match idx {
                8 => c.is_ascii_digit() || c == 'X',
                _ => is_vin_char(c),
            };
            (!ok).then_some(InvalidCharacter {
                character: c,
                position: idx + 1,
            })
        })
        .collect();

    if invalid.is_empty() {
        Ok(())
    } else {
        Err((StructureErrorCode::InvalidCharacters, invalid))
    }
}

/// The world manufacturer identifier: the first three characters, extended with
/// positions 12-14 when the third character is `9`, which marks a low-volume
/// manufacturer that shares its three-character prefix with others.
///
/// # Examples
///
/// ```
/// use corgi_rs::vin::extract_wmi;
/// assert_eq!(extract_wmi("1C6RR7LT2JS179571"), "1C6");
/// assert_eq!(extract_wmi("SC9ABCDE5FCXYZ456"), "SC9XYZ");
/// ```
pub fn extract_wmi(vin: &str) -> String {
    if vin.len() < 3 {
        return vin.to_string();
    }

    let base = &vin[0..3];
    if base.as_bytes()[2] == b'9' && vin.len() >= 14 {
        return [base, &vin[11..14]].concat();
    }

    base.to_string()
}

/// Weights applied to each VIN position when computing the check digit.
/// Position 9 (index 8) is the check digit itself and contributes nothing.
const CHECK_DIGIT_WEIGHTS: [u32; 17] = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2];

/// Transliterate a VIN character to its numeric value for the check digit.
fn transliterate(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        'A'..='H' => Some(c as u32 - 'A' as u32 + 1),
        'J'..='N' => Some(c as u32 - 'J' as u32 + 1),
        'P' => Some(7),
        'R' => Some(9),
        'S'..='Z' => Some(c as u32 - 'S' as u32 + 2),
        _ => None,
    }
}

/// Compute the check digit that position 9 should hold, or `None` when the VIN
/// contains a character the algorithm cannot weigh.
///
/// `is_car_mpv_lt` selects the stricter numeric-only rule NHTSA applies to
/// position 13 of cars, MPVs and light trucks.
///
/// # Examples
///
/// ```
/// use corgi_rs::vin::check_digit;
/// assert_eq!(check_digit("1HGCP26739A060971", true), Some('3'));
/// ```
pub fn check_digit(vin: &str, is_car_mpv_lt: bool) -> Option<char> {
    if vin.chars().count() != 17 {
        return None;
    }

    let chars: Vec<char> = vin.chars().collect();
    let extended_wmi = chars[2] == '9';
    let mut sum = 0u32;

    for (idx, &c) in chars.iter().enumerate() {
        let position = idx + 1;
        let allowed = match position {
            10 => is_model_year_char(c),
            13 if !extended_wmi && is_car_mpv_lt => c.is_ascii_digit(),
            14 if !extended_wmi => c.is_ascii_digit(),
            15..=17 => c.is_ascii_digit(),
            _ => is_vin_char(c),
        };
        if !allowed {
            return None;
        }

        sum += transliterate(c)? * CHECK_DIGIT_WEIGHTS[idx];
    }

    Some(match sum % 11 {
        10 => 'X',
        r => char::from_digit(r, 10)?,
    })
}

/// Whether the VIN's check digit agrees with the computed one.
pub fn check_digit_valid(vin: &str, is_car_mpv_lt: bool) -> bool {
    match (vin.chars().nth(8), check_digit(vin, is_car_mpv_lt)) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => false,
    }
}

/// Whether `c` is a legal position-10 model year code.
pub fn is_model_year_char(c: char) -> bool {
    matches!(c, 'A'..='H' | 'J'..='N' | 'P' | 'R'..='T' | 'V'..='Y' | '1'..='9')
}

/// A model year, plus whether the VIN pinned it down unambiguously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelYear {
    pub year: i32,
    /// Position 10 repeats every 30 years. `conclusive` is false when nothing in
    /// the VIN says which 30-year block applies, so the year is a best guess.
    pub conclusive: bool,
}

/// Decode the model year from position 10.
///
/// The code cycles every 30 years, so for cars, MPVs and light trucks NHTSA
/// disambiguates with position 7: a digit there means the older block. For every
/// other vehicle type the only correction available is to reject a year that is
/// implausibly far in the future.
///
/// `now_year` is the current calendar year; a VIN may legitimately carry a model
/// year up to two years ahead of it.
///
/// # Examples
///
/// ```
/// use corgi_rs::vin::model_year;
/// // Position 7 is a letter, so 'J' is 2018 rather than 1988.
/// let my = model_year("1C6RR7LT2JS179571", true, 2026).unwrap();
/// assert_eq!(my.year, 2018);
/// assert!(my.conclusive);
/// ```
pub fn model_year(vin: &str, is_car_mpv_lt: bool, now_year: i32) -> Option<ModelYear> {
    let code = vin.chars().nth(9)?;

    // The code runs A..H, J..N, P, R..T, V..Y, 1..9 over 2010..=2039.
    let mut year = match code {
        'A'..='H' => 2010 + (code as i32 - 'A' as i32),
        'J'..='N' => 2010 + (code as i32 - 'A' as i32) - 1,
        'P' => 2023,
        'R'..='T' => 2010 + (code as i32 - 'A' as i32) - 3,
        'V'..='Y' => 2010 + (code as i32 - 'A' as i32) - 4,
        '1'..='9' => 2031 + (code as i32 - '1' as i32),
        _ => return None,
    };

    let mut conclusive = false;
    let position_7 = vin.chars().nth(6);

    if is_car_mpv_lt {
        match position_7 {
            Some(c) if c.is_ascii_digit() => {
                year -= 30;
                conclusive = true;
            }
            Some(c) if c.is_ascii_uppercase() => conclusive = true,
            _ => {}
        }
    }

    if !conclusive && year > now_year + 2 {
        year -= 30;
        conclusive = true;
    }

    Some(ModelYear { year, conclusive })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_rejects_wrong_length() {
        assert_eq!(
            validate_structure("1C6RR7LT2JS17957").unwrap_err().0,
            StructureErrorCode::InvalidLength
        );
    }

    #[test]
    fn structure_reports_every_bad_character_with_its_position() {
        let (code, bad) = validate_structure("1C6RR7LTXJS17957I").unwrap_err();
        assert_eq!(code, StructureErrorCode::InvalidCharacters);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].character, 'I');
        assert_eq!(bad[0].position, 17);
    }

    #[test]
    fn structure_allows_x_as_the_check_digit() {
        assert!(validate_structure("1C6RR7LTXJS179571").is_ok());
    }

    #[test]
    fn an_unassigned_model_year_code_is_a_year_problem_not_a_structure_one() {
        // '0' and 'U' are valid VIN characters but not model-year codes.
        assert!(validate_structure("1C6RR7LT20S179571").is_ok());
        assert!(model_year("1C6RR7LT20S179571", true, 2026).is_none());
        assert!(model_year("1C6RR7LT2US179571", true, 2026).is_none());
    }

    #[test]
    fn wmi_extends_for_low_volume_manufacturers() {
        assert_eq!(extract_wmi("1C6RR7LT2JS179571"), "1C6");
        // Third character '9': positions 12-14 join the WMI.
        assert_eq!(extract_wmi("SC9ABCDE5FCXYZ456"), "SC9XYZ");
    }

    #[test]
    fn check_digit_matches_the_nhtsa_reference_vin() {
        // NHTSA publishes 1M8GDM9AXKP042788 as its check-digit worked example.
        assert_eq!(check_digit("1M8GDM9AXKP042788", false), Some('X'));
        assert!(check_digit_valid("1M8GDM9AXKP042788", false));
    }

    #[test]
    fn check_digit_detects_a_transposition() {
        assert!(check_digit_valid("1HGCP26739A060971", true));
        assert!(!check_digit_valid("1HGCP26739A060917", true));
    }

    #[test]
    fn model_year_uses_position_7_to_pick_the_30_year_block() {
        // Letter at position 7 -> current block.
        assert_eq!(
            model_year("1C6RR7LT2JS179571", true, 2026).unwrap(),
            ModelYear {
                year: 2018,
                conclusive: true
            }
        );
        // Digit at position 7 -> previous block.
        assert_eq!(
            model_year("2FTEF14H8TCA73155", true, 2026).unwrap(),
            ModelYear {
                year: 1996,
                conclusive: true
            }
        );
    }

    #[test]
    fn model_year_falls_back_when_the_block_is_unknown() {
        // Heavy truck: position 7 says nothing, so a year past now+2 rolls back.
        let my = model_year("1FUJGLDR39LAB1234", false, 2026).unwrap();
        assert_eq!(my.year, 2009);
        assert!(my.conclusive);
    }

    #[test]
    fn model_year_covers_the_whole_code_alphabet() {
        for (code, expected) in [
            ('A', 2010),
            ('H', 2017),
            ('J', 2018),
            ('N', 2022),
            ('P', 2023),
            ('R', 2024),
            ('T', 2026),
            ('V', 2027),
            ('Y', 2030),
            ('1', 2031),
            ('9', 2039),
        ] {
            let vin = format!("1C6RR7LT2{code}S179571");
            // Position 7 is 'L', a letter, so no 30-year rollback applies.
            assert_eq!(
                model_year(&vin, true, 2040).unwrap().year,
                expected,
                "code {code}"
            );
        }
    }
}
