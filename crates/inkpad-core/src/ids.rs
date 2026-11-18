//! Identificatori opachi. Restano leggibili (`nb-…`, `pg-…`) e sono validi
//! come nome di file: niente input utente nel percorso.

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    #[test]
    fn gli_id_sono_nomi_di_file_sicuri() {
        let id = super::new_id("pg");
        assert!(id.starts_with("pg-"));
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
        assert_ne!(id, super::new_id("pg"));
    }
}
