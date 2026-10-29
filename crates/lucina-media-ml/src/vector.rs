//! Vector helpers: unit length, dot product, and the f16 bytes the catalog stores.

use half::f16;

/// `v / |v|`; a zero or non-finite vector is left as it is and reported `false`.
pub fn normalize(v: &mut [f32]) -> bool {
    let n: f64 = v
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    if !(n.is_finite() && n > 0.0) {
        return false;
    }
    for x in v.iter_mut() {
        *x = (*x as f64 / n) as f32;
    }
    true
}

/// The dot product (the cosine similarity of two unit vectors). Lengths must match.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Little-endian f16, 2 bytes a value: a 768-value vector is 1.5 KB.
pub fn to_f16_bytes(v: &[f32]) -> Vec<u8> {
    v.iter()
        .flat_map(|x| f16::from_f32(*x).to_le_bytes())
        .collect()
}

/// The inverse of [`to_f16_bytes`]; `None` for an odd byte count.
pub fn from_f16_bytes(b: &[u8]) -> Option<Vec<f32>> {
    let (pairs, rest) = b.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    Some(
        pairs
            .iter()
            .map(|c| f16::from_le_bytes(*c).to_f32())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_to_unit_length() {
        let mut v = vec![3.0, 4.0];
        assert!(normalize(&mut v));
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        let mut z = vec![0.0, 0.0];
        assert!(!normalize(&mut z));
        assert_eq!(z, vec![0.0, 0.0]);
        let mut nan = vec![f32::NAN, 1.0];
        assert!(!normalize(&mut nan));
    }

    #[test]
    fn f16_round_trip_keeps_three_decimals() {
        let v = vec![0.0123, -0.5, 0.999, -0.0001];
        let b = to_f16_bytes(&v);
        assert_eq!(b.len(), 8);
        let back = from_f16_bytes(&b).unwrap();
        for (x, y) in v.iter().zip(&back) {
            assert!((x - y).abs() < 1e-3, "{x} vs {y}");
        }
        assert!(from_f16_bytes(&[1, 2, 3]).is_none());
    }

    #[test]
    fn dot_of_unit_vectors_is_cosine() {
        let a = [1.0, 0.0];
        let b = [0.0, 1.0];
        assert_eq!(dot(&a, &b), 0.0);
        assert_eq!(dot(&a, &a), 1.0);
    }
}
