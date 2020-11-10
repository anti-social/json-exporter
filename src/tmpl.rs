use nom::IResult;
use nom::branch::{
    alt,
};
use nom::bytes::complete::{
    is_not,
    tag,
    take_till1,
};
use nom::character::complete::{
    anychar,
    digit1,
};
use nom::combinator::{
    map,
    map_res,
};
use nom::multi::{
    many1,
};
use nom::sequence::{
    delimited,
    preceded,
};


type StrResult<'a, T> = IResult<&'a str, T>;

#[derive(Debug, PartialEq, Clone)]
pub enum Var {
    Ix(u32),
    Ident(String),
}

#[derive(Debug, PartialEq, Clone)]
pub enum Placeholder {
    Text(String),
    Var(Var),
}

fn uint(input: &str) -> IResult<&str, u32> {
    map_res(
    digit1,
        str::parse
  )(input)
}

fn var_ix(input: &str) -> IResult<&str, Var> {
    map(
        uint,
        Var::Ix
    )(input)
}

fn ident(input: &str) -> IResult<&str, String> {
    map(
        preceded(
            tag("."),
            many1(anychar)
        ),
        |chars| chars.iter().collect()
    )(input)
}

fn var_ident(input: &str) -> IResult<&str, Var> {
    map(
        ident,
        Var::Ident
    )(input)
}

fn var(input: &str) -> IResult<&str, Var> {
    alt((var_ix, var_ident))(input)
}

fn var_placeholder(input: &str) -> IResult<&str, Placeholder> {
    let (input, var_str) = delimited(
        tag("${"), is_not("}"), tag("}")
    )(input)?;
    let (_, placeholder) = map(
        var,
        Placeholder::Var
    )(var_str)?;
    Ok((input, placeholder))
}

fn var_simple_placeholder(input: &str) -> StrResult<Placeholder> {
    map(
    preceded(
        tag("$"),
        var_ix
        ),
        Placeholder::Var
    )(input)
}

fn text_placeholder(input: &str) -> IResult<&str, Placeholder> {
    let (input, text) = take_till1(|c| c == '$')(input)?;
    Ok((input, Placeholder::Text(text.to_string())))
}

pub fn string_with_placeholders(input: &str) -> IResult<&str, Vec<Placeholder>> {
    many1(
        alt((
            var_placeholder,
            var_simple_placeholder,
            text_placeholder,
        ))
    )(input)
}

#[cfg(test)]
mod tests {
    use super::{
        Placeholder,
        string_with_placeholders,
        text_placeholder,
        uint,
        var,
        var_placeholder,
        var_simple_placeholder,
        Var,
    };
    use nom::error::Error;
    use nom::error::ErrorKind;

    #[test]
    fn test_uint() {
        assert_eq!(
            uint(""),
            Err(nom::Err::Error(Error { input: "", code: ErrorKind::Digit }))
        );
        assert_eq!(
            uint("0"),
            Ok(("", 0))
        );
        assert_eq!(
            uint("123asdf"),
            Ok(("asdf", 123))
        );
        assert_eq!(
            uint("0123456789"),
            Ok(("", 123456789))
        );
        assert_eq!(
            uint("asdf"),
            Err(nom::Err::Error(Error { input: "asdf", code: ErrorKind::Digit }))
        );
    }

    #[test]
    fn test_var() {
        assert_eq!(
            var("0"),
            Ok(("", Var::Ix(0)))
        );
        assert_eq!(
            var(".asdf"),
            Ok(("", Var::Ident("asdf".to_string())))
        );
    }

    #[test]
    fn test_var_simple_placeholder() {
        assert_eq!(
            var_simple_placeholder("$0"),
            Ok(("", Placeholder::Var(Var::Ix(0))))
        );
        assert_eq!(
            var_simple_placeholder("$0,"),
            Ok((",", Placeholder::Var(Var::Ix(0))))
        );
    }

    #[test]
    fn test_placeholder() {
        assert_eq!(
            var_placeholder("${0}"),
            Ok(("", Placeholder::Var(Var::Ix(0))))
        );
        assert_eq!(
            var_placeholder("${.a.b.c}"),
            Ok(("", Placeholder::Var(Var::Ident("a.b.c".to_string()))))
        );
    }

    #[test]
    fn test_text_placeholder() {
        assert_eq!(
            text_placeholder("Test string"),
            Ok(("", Placeholder::Text("Test string".to_string())))
        );
    }

    #[test]
    fn test_string_with_placeholders() {
        assert_eq!(
            string_with_placeholders(""),
            Err(nom::Err::Error(Error { input: "", code: ErrorKind::TakeTill1 }))
        );
        assert_eq!(
            string_with_placeholders("Test string"),
            Ok(("", vec!(Placeholder::Text("Test string".to_string()))))
        );
        assert_eq!(
            string_with_placeholders("${0}"),
            Ok(("", vec!(Placeholder::Var(Var::Ix(0)))))
        );
        assert_eq!(
            string_with_placeholders("Test string: ${0}"),
            Ok(("", vec!(Placeholder::Text("Test string: ".to_string()), Placeholder::Var(Var::Ix(0)))))
        );
        assert_eq!(
            string_with_placeholders("Indexes: ${1} - $0, variable: ${.user.name}"),
            Ok((
                "",
                vec!(
                    Placeholder::Text("Indexes: ".to_string()),
                    Placeholder::Var(Var::Ix(1)),
                    Placeholder::Text(" - ".to_string()),
                    Placeholder::Var(Var::Ix(0)),
                    Placeholder::Text(", variable: ".to_string()),
                    Placeholder::Var(Var::Ident("user.name".to_string())),
                )
            ))
        );
    }
}