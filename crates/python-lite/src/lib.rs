//! python-lite: A minimal Python-like interpreter for ClaudioOS.
//!
//! This is a `no_std` + `alloc` tree-walking interpreter that supports a useful
//! subset of Python syntax: variables, math, strings, lists, control flow,
//! functions, and basic builtins.
//!
//! It is NOT CPython-compatible. It is "Python-shaped" enough that an AI agent
//! can write useful scripts and execute them on bare metal.

#![no_std]

extern crate alloc;

mod tokenizer;
mod parser;
mod eval;

pub use eval::{Interpreter, Value};

use alloc::string::String;

/// Execute Python-lite source code and return captured print() output.
///
/// This is the main entry point for tool integration.
pub fn execute(source: &str) -> Result<String, String> {
    let tokens = tokenizer::tokenize(source)?;
    let ast = parser::parse(tokens)?;
    let mut interp = Interpreter::new();
    interp.exec_block(&ast)?;
    Ok(interp.take_output())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello_world() {
        let out = execute("print(\"hello world\")").unwrap();
        assert_eq!(out.trim(), "hello world");
    }

    #[test]
    fn test_variables_and_math() {
        let out = execute("x = 2 + 3\nprint(x)").unwrap();
        assert_eq!(out.trim(), "5");
    }

    #[test]
    fn test_string_concat() {
        let out = execute("a = \"hello\"\nb = \" world\"\nprint(a + b)").unwrap();
        assert_eq!(out.trim(), "hello world");
    }

    #[test]
    fn test_if_else() {
        let code = "x = 10\nif x > 5:\n    print(\"big\")\nelse:\n    print(\"small\")";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "big");
    }

    #[test]
    fn test_for_loop() {
        let code = "for i in range(3):\n    print(i)";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "0\n1\n2");
    }

    #[test]
    fn test_while_loop() {
        let code = "x = 0\nwhile x < 3:\n    print(x)\n    x = x + 1";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "0\n1\n2");
    }

    #[test]
    fn test_function_def() {
        let code = "def add(a, b):\n    return a + b\nprint(add(3, 4))";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "7");
    }

    #[test]
    fn test_len_builtin() {
        let out = execute("print(len(\"hello\"))").unwrap();
        assert_eq!(out.trim(), "5");
    }

    #[test]
    fn test_list_operations() {
        let code = "xs = [1, 2, 3]\nprint(len(xs))\nprint(xs[1])";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "3\n2");
    }

    #[test]
    fn test_float_math() {
        let out = execute("print(3.14 * 2)").unwrap();
        assert_eq!(out.trim(), "6.28");
    }

    #[test]
    fn test_power_operator() {
        let out = execute("print(2 ** 10)").unwrap();
        assert_eq!(out.trim(), "1024");
    }

    #[test]
    fn test_modulo() {
        let out = execute("print(17 % 5)").unwrap();
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn test_nested_function() {
        let code = "def fib(n):\n    if n <= 1:\n        return n\n    return fib(n - 1) + fib(n - 2)\nprint(fib(8))";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "21");
    }

    #[test]
    fn test_string_multiply() {
        let out = execute("print(\"ab\" * 3)").unwrap();
        assert_eq!(out.trim(), "ababab");
    }

    #[test]
    fn test_type_builtin() {
        let out = execute("print(type(42))").unwrap();
        assert_eq!(out.trim(), "int");
    }

    #[test]
    fn test_str_int_builtins() {
        let out = execute("print(int(\"42\") + 1)\nprint(str(100))").unwrap();
        assert_eq!(out.trim(), "43\n100");
    }

    #[test]
    fn test_elif() {
        let code = "x = 5\nif x > 10:\n    print(\"a\")\nelif x > 3:\n    print(\"b\")\nelse:\n    print(\"c\")";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "b");
    }

    #[test]
    fn test_comparison_chain() {
        let out = execute("print(1 < 2)\nprint(3 >= 3)\nprint(4 == 5)").unwrap();
        assert_eq!(out.trim(), "True\nTrue\nFalse");
    }

    #[test]
    fn test_boolean_logic() {
        let out = execute("print(True and False)\nprint(True or False)\nprint(not True)").unwrap();
        assert_eq!(out.trim(), "False\nTrue\nFalse");
    }

    #[test]
    fn test_list_append() {
        let code = "xs = []\nxs.append(1)\nxs.append(2)\nprint(xs)";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "[1, 2]");
    }

    #[test]
    fn test_multiline_string() {
        let out = execute("print(\"hello\" + \" \" + \"world\")").unwrap();
        assert_eq!(out.trim(), "hello world");
    }

    #[test]
    fn test_negative_numbers() {
        let out = execute("print(-5 + 3)").unwrap();
        assert_eq!(out.trim(), "-2");
    }

    #[test]
    fn test_integer_division() {
        let out = execute("print(7 // 2)").unwrap();
        assert_eq!(out.trim(), "3");
    }

    #[test]
    fn test_none_value() {
        let out = execute("x = None\nprint(x)").unwrap();
        assert_eq!(out.trim(), "None");
    }

    #[test]
    fn test_break_in_loop() {
        let code = "for i in range(10):\n    if i == 3:\n        break\n    print(i)";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "0\n1\n2");
    }

    #[test]
    fn test_continue_in_loop() {
        let code = "for i in range(5):\n    if i == 2:\n        continue\n    print(i)";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "0\n1\n3\n4");
    }

    #[test]
    fn test_multiple_print_args() {
        let out = execute("print(\"x:\", 42, \"y:\", 3.14)").unwrap();
        assert_eq!(out.trim(), "x: 42 y: 3.14");
    }

    #[test]
    fn test_augmented_assignment() {
        let code = "x = 10\nx += 5\nx -= 2\nprint(x)";
        let out = execute(code).unwrap();
        assert_eq!(out.trim(), "13");
    }
}
