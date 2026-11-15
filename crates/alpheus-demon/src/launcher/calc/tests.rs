use super::*;

fn ok(q: &str) -> String {
    evaluate(q).unwrap().unwrap().text
}

fn err(q: &str) -> CalcError {
    evaluate(q).unwrap().unwrap_err()
}

#[test]
fn arithmetic_and_precedence() {
    for (q, want) in [
        ("1+2*3", "7"),
        ("(1+2)*3", "9"),
        ("2^3^2", "512"),
        ("-2^2", "-4"),
        ("7/2", "3.5"),
        ("10/4*2", "5"),
        ("7 % 3", "1"),
        ("2x3", "6"),
        ("2 x 3", "6"),
        ("1_000 * 3", "3000"),
        ("1e3 + 1", "1001"),
        ("0.1+0.2", "0.3"),
        ("2^-1", "0.5"),
    ] {
        assert_eq!(ok(q), want, "{q}");
    }
}

#[test]
fn functions_and_constants() {
    for (q, want) in [
        ("sqrt(16)", "4"),
        ("cbrt(27)", "3"),
        ("abs(-3)", "3"),
        ("ln(e)", "1"),
        ("log(1000)", "3"),
        ("log2(8)", "3"),
        ("exp(0)", "1"),
        ("sin(0)", "0"),
        ("cos(pi)", "-1"),
        ("atan(1)*4", "3.14159265359"),
        ("tau/2", "3.14159265359"),
        ("round(2.5)", "3"),
        ("floor(-1.5)", "-2"),
        ("ceil(1.2)", "2"),
        ("min(3, 1, 2)", "1"),
        ("max(3,1,2)", "3"),
    ] {
        assert_eq!(ok(q), want, "{q}");
    }
}

#[test]
fn percent_of() {
    assert_eq!(ok("20% of 150"), "30");
}

#[test]
fn formatting() {
    for (q, want) in
        [("1/3", "0.333333333333"), ("2^64", "18446744073709551616"), ("2^200", "1.60693804426e60"), ("1e-12", "1e-12"), ("0-0", "0")]
    {
        assert_eq!(ok(q), want, "{q}");
    }
}

#[test]
fn bases_only_for_other_base_integers() {
    let a = evaluate("0xff + 1").unwrap().unwrap();
    assert_eq!(a, Answer { text: "256".into(), bases: Some(("0x100".into(), "0b100000000".into())) });
    assert_eq!(a.shown(), "= 256 · 0x100 · 0b100000000");
    assert_eq!(evaluate("0b1010").unwrap().unwrap().bases, Some(("0xa".into(), "0b1010".into())));
    assert_eq!(evaluate("0o17").unwrap().unwrap().bases, Some(("0xf".into(), "0b1111".into())));
    for q in ["255 + 1", "0xff / 2", "0x1 - 0x2"] {
        assert_eq!(evaluate(q).unwrap().unwrap().bases, None, "{q}");
    }
    assert_eq!(evaluate("255 + 1").unwrap().unwrap().shown(), "= 256");
}

#[test]
fn errors() {
    assert_eq!(err("1/0"), CalcError::DivisionByZero);
    assert_eq!(err("5 % 0"), CalcError::DivisionByZero);
    assert_eq!(err("2^2000"), CalcError::Overflow);
    assert_eq!(err("sqrt(-1)"), CalcError::Domain);
    assert_eq!(err("ln(0)"), CalcError::Domain);
    let nested = |n: usize| format!("{}1{}", "(".repeat(n), ")".repeat(n));
    assert_eq!(err(&nested(65)), CalcError::Nesting);
    assert_eq!(ok(&nested(64)), "1");
    assert_eq!(format!("{}", CalcError::DivisionByZero), "division by zero");
    assert_eq!(format!("{}", CalcError::Overflow), "overflow");
    assert_eq!(format!("{}", CalcError::Domain), "not a real number");
    assert_eq!(format!("{}", CalcError::Nesting), "too deeply nested");
}

#[test]
fn not_expressions() {
    for q in ["", "firefox", "1+", "(1", "2 pi", "sqrt", "min()", "sqrt(1, 2)"] {
        assert!(evaluate(q).is_none(), "{q:?}");
    }
    let long = |n: usize| format!("{}1", "1+".repeat(n));
    assert_eq!(long(128).chars().count(), 257);
    assert!(evaluate(&long(128)).is_none());
    assert_eq!(long(127).chars().count(), 255);
    assert!(evaluate(&long(127)).is_some());
}

#[test]
fn looks_like_math_accepts_expressions() {
    for q in ["1+2", "0xff", "2^10", "sqrt(2)", "20% of 150", "1e3", "5 km to mi"] {
        assert!(looks_like_math(q), "{q}");
    }
}

#[test]
fn looks_like_math_refuses_words_and_plain_numbers() {
    for q in ["e", "pi", "2048", "42", "3.5", "1_000", "firefox", "7zip", "1password", "mp3", ""] {
        assert!(!looks_like_math(q), "{q:?}");
    }
}

#[test]
fn looks_like_math_refuses_signed_and_parenthesised_plain_numbers() {
    for q in ["-5", "+5", "(5)", "-(5)", "-3.5", "( 42 )"] {
        assert!(!looks_like_math(q), "{q:?}");
    }
    for q in ["(5", "5)", "-5+1", "(5)+1"] {
        assert!(looks_like_math(q), "{q:?}");
    }
}

#[test]
fn hostile_inputs_neither_panic_nor_crawl() {
    let start = std::time::Instant::now();
    let minuses = format!("{}1", "-".repeat(255));
    assert_eq!(ok(&minuses), "-1");
    assert_eq!(err("9^9^9^9"), CalcError::Overflow);
    // i128::MAX + 1 leaves the exact range: a float, or an error, never a panic.
    let r = evaluate("170141183460469231731687303715884105727 + 1").unwrap();
    assert!(r.is_ok() || r.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn the_remainder_is_truncated() {
    assert_eq!(ok("-7 % 3"), "-1");
    assert_eq!(ok("7 % -3"), "1");
}
