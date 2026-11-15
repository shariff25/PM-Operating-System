//! The stats tools (`stats.sample_size`, `stats.compare`): deterministic experiment maths,
//! so a skill quotes a computed number instead of a guessed one (`docs/pm-craft-spec.md` §5).
//!
//! No connector and no external data: the result is computed here from the arguments, so it
//! is returned as is, not wrapped as untrusted content. The service still checks the skill
//! declares the tool and audits the call like any read.
//!
//! The normal distribution is implemented here rather than pulled in as a dependency: the
//! CDF from an accurate `erfc` (a power series near zero, a continued fraction in the tail),
//! the quantile from Acklam's rational approximation with one Halley refinement step, which
//! brings it to about machine precision.

use serde_json::{json, Map, Value};

/// The most arms (control included) `stats.sample_size` sizes for.
pub const MAX_VARIANTS: u64 = 10;

/// Run one stats tool by its action (`sample_size`, `compare`). An `Err` is an invalid
/// argument, said in a sentence the model can act on.
pub fn call(action: &str, args: &Value) -> Result<Value, String> {
    match action {
        "sample_size" => sample_size(args),
        "compare" => compare(args),
        other => Err(format!("unknown stats tool `stats.{other}`")),
    }
}

// ----- the normal distribution ---------------------------------------------------------

const SQRT_PI: f64 = 1.772_453_850_905_516;
const SQRT_2: f64 = std::f64::consts::SQRT_2;
const SQRT_2PI: f64 = 2.506_628_274_631_000_7;

/// The complementary error function, to about 1e-15 relative error.
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    if x < 2.5 {
        1.0 - erf_series(x)
    } else if x > 27.0 {
        0.0
    } else {
        erfc_continued_fraction(x)
    }
}

/// `erf(x)` for `0 <= x < 2.5`: `2/√π · e^(−x²) · Σ 2ⁿ x^(2n+1) / (1·3·…·(2n+1))`. Every
/// term is positive, so there is no cancellation.
fn erf_series(x: f64) -> f64 {
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    let mut n = 0.0;
    while term > sum * 1e-17 {
        n += 1.0;
        term *= 2.0 * x2 / (2.0 * n + 1.0);
        sum += term;
    }
    2.0 / SQRT_PI * (-x2).exp() * sum
}

/// `erfc(x)` for `x >= 2.5`, from `e^(−x²)/√π · 1/(x + (1/2)/(x + 1/(x + (3/2)/(x + …))))`,
/// evaluated with the modified Lentz method.
fn erfc_continued_fraction(x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let mut f = x;
    let mut c = x;
    let mut d = 0.0;
    for k in 1..500 {
        let a = k as f64 / 2.0;
        d = x + a * d;
        d = if d.abs() < TINY { TINY } else { d };
        c = x + a / c;
        c = if c.abs() < TINY { TINY } else { c };
        d = 1.0 / d;
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < 1e-16 {
            break;
        }
    }
    (-x * x).exp() / SQRT_PI / f
}

/// The standard normal CDF, `Φ(x)`.
pub fn normal_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / SQRT_2)
}

/// The standard normal quantile, `Φ⁻¹(p)` for `0 < p < 1`: Acklam's approximation (relative
/// error about 1e-9), then one Halley step against [`normal_cdf`].
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const P_LOW: f64 = 0.02425;

    if !(p > 0.0 && p < 1.0) {
        return match p {
            0.0 => f64::NEG_INFINITY,
            1.0 => f64::INFINITY,
            _ => f64::NAN,
        };
    }
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let x = if p < P_LOW {
        tail((-2.0 * p.ln()).sqrt())
    } else if p <= 1.0 - P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    };
    // One Halley step.
    let e = normal_cdf(x) - p;
    let u = e * SQRT_2PI * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

// ----- arguments -----------------------------------------------------------------------

fn number(args: &Value, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() => Ok(Some(n)),
            _ => Err(format!("`{key}` must be a number")),
        },
    }
}

fn count(args: &Value, key: &str, what: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{what}` must be a whole number, 0 or more")),
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a string")),
    }
}

fn alpha(args: &Value) -> Result<f64, String> {
    let alpha = number(args, "alpha")?.unwrap_or(0.05);
    if !(alpha > 0.0 && alpha < 1.0) {
        return Err(format!("`alpha` must be between 0 and 1 (got {alpha})"));
    }
    Ok(alpha)
}

/// Round to `places` decimal places, for readable output.
fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

/// Round to `digits` significant figures, so a tiny p-value doesn't print as 0.
fn round_sig(x: f64, digits: i32) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let places = digits - 1 - x.abs().log10().floor() as i32;
    round(x, places.max(0))
}

/// A sample size from the formula: rounded up, but not past floating-point noise (a value a
/// hair above a whole number is that number).
fn ceil_n(n: f64) -> u64 {
    let nearest = n.round();
    if (n - nearest).abs() < 1e-9 * n.max(1.0) {
        nearest as u64
    } else {
        n.ceil() as u64
    }
}

// ----- stats.sample_size ---------------------------------------------------------------

/// Users needed per arm to detect `mde` with the given error rates.
///
/// - Proportion: the two-proportion z-test with unpooled variance,
///   `n = (z_{α/2} + z_β)² · (p1(1−p1) + p2(1−p2)) / (p2 − p1)²`.
/// - Mean: `n = 2 · (z_{α/2} + z_β)² · σ² / δ²`.
///
/// With more than two arms, each treatment arm is compared with control at `alpha`, and
/// `total` is `per_arm` times the number of arms.
pub fn sample_size(args: &Value) -> Result<Value, String> {
    let metric = text(args, "metric")?.ok_or("`metric` is required: `proportion` or `mean`")?;
    if !matches!(metric, "proportion" | "mean") {
        return Err(format!(
            "`metric` must be `proportion` or `mean` (got `{metric}`)"
        ));
    }
    let baseline = number(args, "baseline")?.ok_or("`baseline` is required")?;
    let mde = number(args, "mde")?.ok_or("`mde` is required")?;
    if mde <= 0.0 {
        return Err(format!(
            "`mde` (the minimum detectable effect) must be more than 0 (got {mde})"
        ));
    }
    let mde_kind = text(args, "mde_kind")?.unwrap_or("relative");
    if !matches!(mde_kind, "relative" | "absolute") {
        return Err(format!(
            "`mde_kind` must be `relative` or `absolute` (got `{mde_kind}`)"
        ));
    }
    let alpha = alpha(args)?;
    let power = number(args, "power")?.unwrap_or(0.8);
    if !(power > 0.0 && power < 1.0) {
        return Err(format!("`power` must be between 0 and 1 (got {power})"));
    }
    let two_sided = match args.get("two_sided") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("`two_sided` must be true or false".into()),
    };
    let variants = count(args, "variants", "variants")?.unwrap_or(2);
    if !(2..=MAX_VARIANTS).contains(&variants) {
        return Err(format!(
            "`variants` is the number of arms including control: 2 to {MAX_VARIANTS} (got {variants})"
        ));
    }
    let daily_users = count(args, "daily_users", "daily_users")?;
    if daily_users == Some(0) {
        return Err("`daily_users` must be at least 1".into());
    }

    let z_alpha = normal_quantile(1.0 - if two_sided { alpha / 2.0 } else { alpha });
    let z_beta = normal_quantile(power);
    let z = z_alpha + z_beta;

    let mut assumptions = Map::new();
    assumptions.insert("metric".into(), json!(metric));
    assumptions.insert("baseline".into(), json!(baseline));
    assumptions.insert("mde".into(), json!(mde));
    assumptions.insert("mde_kind".into(), json!(mde_kind));

    let per_arm = if metric == "proportion" {
        if number(args, "std_dev")?.is_some() {
            return Err(
                "`std_dev` is for a `mean` metric; a proportion's variance comes from its rate"
                    .into(),
            );
        }
        if !(baseline > 0.0 && baseline < 1.0) {
            return Err(format!(
                "a proportion's `baseline` is a rate between 0 and 1, exclusive (got {baseline})"
            ));
        }
        let p1 = baseline;
        let p2 = match mde_kind {
            "relative" => p1 * (1.0 + mde),
            _ => p1 + mde,
        };
        if !(p2 > 0.0 && p2 < 1.0) {
            return Err(format!(
                "the target rate ({}) must be between 0 and 1: make `mde` smaller",
                round_sig(p2, 6)
            ));
        }
        let n = z * z * (p1 * (1.0 - p1) + p2 * (1.0 - p2)) / ((p2 - p1) * (p2 - p1));
        assumptions.insert("target".into(), json!(round_sig(p2, 10)));
        assumptions.insert("absolute_effect".into(), json!(round_sig(p2 - p1, 10)));
        assumptions.insert(
            "test".into(),
            json!("two-proportion z-test, unpooled variance"),
        );
        ceil_n(n)
    } else {
        let sd = number(args, "std_dev")?
            .ok_or("`std_dev` is required for a `mean` metric (the metric's standard deviation)")?;
        if sd <= 0.0 {
            return Err(format!("`std_dev` must be more than 0 (got {sd})"));
        }
        let delta = match mde_kind {
            "relative" => baseline.abs() * mde,
            _ => mde,
        };
        if delta <= 0.0 {
            return Err(
                "a relative `mde` needs a non-zero `baseline` mean; give an absolute `mde` instead"
                    .into(),
            );
        }
        let n = 2.0 * z * z * sd * sd / (delta * delta);
        assumptions.insert("std_dev".into(), json!(sd));
        assumptions.insert("target".into(), json!(round_sig(baseline + delta, 10)));
        assumptions.insert("absolute_effect".into(), json!(round_sig(delta, 10)));
        assumptions.insert("test".into(), json!("two-sample z-test on means"));
        ceil_n(n)
    };
    let total = per_arm * variants;

    assumptions.insert("alpha".into(), json!(alpha));
    assumptions.insert("power".into(), json!(power));
    assumptions.insert("two_sided".into(), json!(two_sided));
    assumptions.insert("variants".into(), json!(variants));
    assumptions.insert("z_alpha".into(), json!(round(z_alpha, 6)));
    assumptions.insert("z_beta".into(), json!(round(z_beta, 6)));

    let mut note = format!(
        "{per_arm} users per arm, {total} in all across {variants} arms, to detect the effect with {}% power at alpha {alpha}{}.",
        round(power * 100.0, 2),
        if two_sided { " (two-sided)" } else { " (one-sided)" },
    );
    if variants > 2 {
        note.push_str(&format!(
            " Each of the {} treatment arms is compared with control at alpha {alpha}; with no correction for multiple comparisons, expect more false positives.",
            variants - 1
        ));
    }
    let mut out = json!({
        "per_arm": per_arm,
        "total": total,
    });
    if let Some(daily) = daily_users {
        let days = total.div_ceil(daily);
        let weeks = days.div_ceil(7);
        assumptions.insert("daily_users".into(), json!(daily));
        out["days"] = json!(days);
        note.push_str(&format!(
            " At {daily} users a day that is {days} day{}; run whole weeks so every weekday is covered: plan {weeks} week{} ({} days).",
            if days == 1 { "" } else { "s" },
            if weeks == 1 { "" } else { "s" },
            weeks * 7
        ));
    }
    note.push_str(" Decide the sample size before the test starts and don't stop early on a good-looking result.");
    out["assumptions"] = Value::Object(assumptions);
    out["note"] = json!(note);
    Ok(out)
}

// ----- stats.compare -------------------------------------------------------------------

struct Arm {
    users: u64,
    conversions: u64,
}

fn arm(args: &Value, key: &str) -> Result<Arm, String> {
    let v = args
        .get(key)
        .filter(|v| v.is_object())
        .ok_or_else(|| format!("`{key}` is required: {{users, conversions}}"))?;
    if let Some(extra) = v.as_object().and_then(|o| {
        o.keys()
            .find(|k| !matches!(k.as_str(), "users" | "conversions"))
    }) {
        return Err(format!("unknown field `{key}.{extra}`"));
    }
    let users = count(v, "users", &format!("{key}.users"))?
        .ok_or_else(|| format!("`{key}.users` is required"))?;
    let conversions = count(v, "conversions", &format!("{key}.conversions"))?
        .ok_or_else(|| format!("`{key}.conversions` is required"))?;
    if users == 0 {
        return Err(format!("`{key}.users` must be at least 1"));
    }
    if conversions > users {
        return Err(format!(
            "`{key}.conversions` ({conversions}) is more than `{key}.users` ({users})"
        ));
    }
    Ok(Arm { users, conversions })
}

/// Compare two conversion rates: a two-proportion z-test, pooled for the p-value and
/// unpooled for the confidence interval of the absolute lift.
pub fn compare(args: &Value) -> Result<Value, String> {
    let control = arm(args, "control")?;
    let treatment = arm(args, "treatment")?;
    let alpha = alpha(args)?;

    let (n1, n2) = (control.users as f64, treatment.users as f64);
    let p1 = control.conversions as f64 / n1;
    let p2 = treatment.conversions as f64 / n2;
    let diff = p2 - p1;

    let pooled = (control.conversions + treatment.conversions) as f64 / (n1 + n2);
    let se_pooled = (pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2)).sqrt();
    let p_value = if se_pooled > 0.0 {
        erfc((diff / se_pooled).abs() / SQRT_2).min(1.0)
    } else {
        // Every user converted, or none did, in both arms: no difference to test.
        1.0
    };
    let se = (p1 * (1.0 - p1) / n1 + p2 * (1.0 - p2) / n2).sqrt();
    let z = normal_quantile(1.0 - alpha / 2.0);
    let significant = p_value < alpha;

    let confidence = round((1.0 - alpha) * 100.0, 4);
    let mut note = format!(
        "{}: p = {} at alpha {alpha}; the {confidence}% interval for the absolute lift is {} to {}.",
        if significant {
            "Significant"
        } else {
            "Not significant"
        },
        round_sig(p_value, 4),
        round(diff - z * se, 6),
        round(diff + z * se, 6),
    );
    note.push_str(
        " A result read before the experiment reached its planned sample size is not reliable: peeking inflates false positives.",
    );
    Ok(json!({
        "control_rate": round(p1, 6),
        "treatment_rate": round(p2, 6),
        "absolute_lift": round(diff, 6),
        "relative_lift": if p1 > 0.0 { json!(round(diff / p1, 6)) } else { Value::Null },
        "ci_low": round(diff - z * se, 6),
        "ci_high": round(diff + z * se, 6),
        "p_value": round_sig(p_value, 4),
        "significant": significant,
        "note": note,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn erfc_matches_reference_values() {
        // Reference values to 16 digits (Abramowitz and Stegun; checked with mpmath).
        let cases = [
            (0.0, 1.0),
            (0.5, 0.479_500_122_186_953_5),
            (1.0, 0.157_299_207_050_285_1),
            (2.0, 4.677_734_981_047_266e-3),
            (2.5, 4.069_520_174_449_59e-4),
            (3.0, 2.209_049_699_858_544e-5),
            (5.0, 1.537_459_794_428_035e-12),
            (10.0, 2.088_487_583_762_545e-45),
        ];
        for (x, want) in cases {
            let got = erfc(x);
            assert!(
                ((got - want) / want).abs() < 1e-13,
                "erfc({x}) = {got:e}, want {want:e}"
            );
        }
        assert!(close(erfc(-1.0), 2.0 - 0.157_299_207_050_285_1, 1e-15));
    }

    #[test]
    fn normal_quantile_inverts_the_cdf() {
        assert!(close(normal_quantile(0.975), 1.959_963_984_540_054, 1e-12));
        assert!(close(normal_quantile(0.8), 0.841_621_233_572_914_4, 1e-12));
        assert!(close(normal_quantile(0.95), 1.644_853_626_951_472_7, 1e-12));
        assert!(close(normal_quantile(0.5), 0.0, 1e-15));
        assert!(close(
            normal_quantile(0.001),
            -3.090_232_306_167_813_5,
            1e-11
        ));
        for p in [1e-10, 0.01, 0.02425, 0.3, 0.7, 0.97575, 0.999_999] {
            let x = normal_quantile(p);
            assert!(((normal_cdf(x) - p) / p).abs() < 1e-12, "p = {p}");
        }
        assert!(normal_quantile(0.0).is_infinite() && normal_quantile(1.5).is_nan());
    }

    #[test]
    fn proportion_sample_size_matches_the_textbook() {
        // 10% baseline, 10% relative lift (to 11%), alpha 0.05 two-sided, power 0.8:
        // (1.959964 + 0.841621)² · (0.09 + 0.0979) / 0.01² = 14748.05, so 14749 per arm.
        let out =
            sample_size(&json!({"metric": "proportion", "baseline": 0.10, "mde": 0.10})).unwrap();
        assert_eq!(out["per_arm"], 14749);
        assert_eq!(out["total"], 29498);
        assert!(out.get("days").is_none());
        assert_eq!(out["assumptions"]["target"], 0.11);
        assert_eq!(out["assumptions"]["two_sided"], true);
        assert_eq!(out["assumptions"]["variants"], 2);
        // The same effect written as absolute.
        let abs = sample_size(&json!({
            "metric": "proportion", "baseline": 0.10, "mde": 0.01, "mde_kind": "absolute"
        }))
        .unwrap();
        assert_eq!(abs["per_arm"], 14749);
        // One-sided needs fewer users; more power needs more.
        let one = sample_size(&json!({
            "metric": "proportion", "baseline": 0.10, "mde": 0.10, "two_sided": false
        }))
        .unwrap();
        assert!(one["per_arm"].as_u64().unwrap() < 14749);
        let more = sample_size(&json!({
            "metric": "proportion", "baseline": 0.10, "mde": 0.10, "power": 0.9
        }))
        .unwrap();
        // (1.959964 + 1.281552)² · 0.1879 / 0.0001 = 19743.4
        assert_eq!(more["per_arm"], 19744);
    }

    #[test]
    fn mean_sample_size() {
        // σ = 10, δ = 1: 2 · 2.801585² · 100 / 1 = 1569.78, so 1570 per arm.
        let out = sample_size(&json!({
            "metric": "mean", "baseline": 50.0, "std_dev": 10.0, "mde": 1.0, "mde_kind": "absolute"
        }))
        .unwrap();
        assert_eq!(out["per_arm"], 1570);
        assert_eq!(out["total"], 3140);
        // The same effect as 2% of a baseline of 50.
        let rel = sample_size(&json!({
            "metric": "mean", "baseline": 50.0, "std_dev": 10.0, "mde": 0.02
        }))
        .unwrap();
        assert_eq!(rel["per_arm"], 1570);
    }

    #[test]
    fn days_and_whole_weeks() {
        let out = sample_size(&json!({
            "metric": "proportion", "baseline": 0.10, "mde": 0.10, "daily_users": 2000
        }))
        .unwrap();
        // 29498 / 2000 = 14.7, so 15 days; whole weeks: 21.
        assert_eq!(out["days"], 15);
        assert_eq!(out["assumptions"]["daily_users"], 2000);
        let note = out["note"].as_str().unwrap();
        assert!(
            note.contains("whole weeks") && note.contains("21 days"),
            "{note}"
        );
    }

    #[test]
    fn more_variants_multiply_the_total() {
        let out = sample_size(&json!({
            "metric": "proportion", "baseline": 0.10, "mde": 0.10, "variants": 3,
            "daily_users": 2000
        }))
        .unwrap();
        assert_eq!(out["per_arm"], 14749);
        assert_eq!(out["total"], 44247);
        assert_eq!(out["days"], 23);
        assert!(out["note"].as_str().unwrap().contains("2 treatment arms"));
    }

    #[test]
    fn sample_size_refuses_bad_input() {
        let bad = [
            json!({"metric": "proportion", "baseline": 0.10, "mde": 0.0}),
            json!({"metric": "proportion", "baseline": 0.10, "mde": -0.1}),
            json!({"metric": "proportion", "baseline": 0.0, "mde": 0.1}),
            json!({"metric": "proportion", "baseline": 1.0, "mde": 0.1}),
            json!({"metric": "proportion", "baseline": 1.5, "mde": 0.1}),
            json!({"metric": "proportion", "baseline": 0.6, "mde": 1.0}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "alpha": 0.0}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "power": 1.0}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "variants": 1}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "variants": 11}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "daily_users": 0}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "mde_kind": "pct"}),
            json!({"metric": "proportion", "baseline": 0.1, "mde": 0.1, "std_dev": 1.0}),
            json!({"metric": "mean", "baseline": 10.0, "mde": 1.0}),
            json!({"metric": "mean", "baseline": 10.0, "std_dev": 0.0, "mde": 1.0}),
            json!({"metric": "mean", "baseline": 0.0, "std_dev": 1.0, "mde": 0.1}),
            json!({"metric": "median", "baseline": 0.1, "mde": 0.1}),
            json!({"metric": "proportion", "baseline": "ten", "mde": 0.1}),
            json!({"baseline": 0.1, "mde": 0.1}),
        ];
        for args in bad {
            assert!(sample_size(&args).is_err(), "{args}");
        }
    }

    #[test]
    fn compare_matches_the_textbook() {
        // 10.0% vs 13.0% on 1000 users each: pooled z = 2.1027, p = 0.0355.
        let out = compare(&json!({
            "control": {"users": 1000, "conversions": 100},
            "treatment": {"users": 1000, "conversions": 130}
        }))
        .unwrap();
        assert_eq!(out["control_rate"], 0.1);
        assert_eq!(out["treatment_rate"], 0.13);
        assert_eq!(out["absolute_lift"], 0.03);
        assert_eq!(out["relative_lift"], 0.3);
        assert_eq!(out["p_value"], 0.03549);
        assert_eq!(out["significant"], true);
        assert_eq!(out["ci_low"], 0.002068);
        assert_eq!(out["ci_high"], 0.057932);
        assert!(out["note"]
            .as_str()
            .unwrap()
            .contains("planned sample size"));
        // At a stricter alpha the same result isn't significant.
        let strict = compare(&json!({
            "control": {"users": 1000, "conversions": 100},
            "treatment": {"users": 1000, "conversions": 130},
            "alpha": 0.01
        }))
        .unwrap();
        assert_eq!(strict["significant"], false);
        assert!(strict["ci_low"].as_f64().unwrap() < 0.0);
    }

    #[test]
    fn compare_edge_cases() {
        let none = compare(&json!({
            "control": {"users": 10, "conversions": 0},
            "treatment": {"users": 10, "conversions": 0}
        }))
        .unwrap();
        assert_eq!(none["p_value"], 1.0);
        assert_eq!(none["significant"], false);
        assert!(none["relative_lift"].is_null());
    }

    #[test]
    fn compare_refuses_bad_input() {
        let bad = [
            json!({"control": {"users": 0, "conversions": 0}, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10, "conversions": 11}, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10, "conversions": -1}, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10.5, "conversions": 1}, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10}, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10, "conversions": 1, "rate": 0.1}, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10, "conversions": 1}}),
            json!({"control": 5, "treatment": {"users": 10, "conversions": 1}}),
            json!({"control": {"users": 10, "conversions": 1}, "treatment": {"users": 10, "conversions": 1}, "alpha": 1.0}),
        ];
        for args in bad {
            assert!(compare(&args).is_err(), "{args}");
        }
    }
}
