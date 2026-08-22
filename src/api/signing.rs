pub struct SigningContext<'a> {
    pub method: &'a str,
    pub path_and_query: &'a str,
}
