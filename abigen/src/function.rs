// Copyright 2015-2019 Parity Technologies
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use heck::ToSnakeCase;
use proc_macro2::{Span, TokenStream};
use quote::quote;

use super::{
    from_template_param, from_token, get_output_kinds, get_template_names, input_names, rust_type,
    template_param_type, to_ethabi_param_vec, to_token,
};

struct TemplateParam {
    /// Template param declaration.
    ///
    /// ```text
    /// [T0: Into<Uint>, T1: Into<Bytes>, T2: IntoIterator<Item = U2>, U2 = Into<Uint>]
    /// ```
    declaration: TokenStream,
    /// Template param definition.
    ///
    /// ```text
    /// [param0: T0, hello_world: T1, param2: T2]
    /// ```
    definition: TokenStream,
}

struct Inputs {
    /// Collects template params into vector.
    ///
    /// ```text
    /// [Token::Uint(param0.into()), Token::Bytes(hello_world.into()), Token::Array(param2.into_iter().map(Into::into).collect())]
    /// ```
    tokenize: Vec<TokenStream>,
    /// Template params.
    template_params: Vec<TemplateParam>,
    /// Quote used to recreate `Vec<ethabi::Param>`
    recreate_quote: TokenStream,
}

struct Outputs {
    /// Decoding implementation.
    implementation: TokenStream,
    /// Decode result.
    result: TokenStream,
    /// Quote used to recreate `Vec<ethabi::Param>`.
    recreate_quote: TokenStream,
}

/// Structure used to generate contract's function interface.
pub struct Function {
    /// Function name.
    name: String,
    /// Function input params.
    inputs: Inputs,
    /// Function output params.
    outputs: Outputs,
    #[deprecated(
        note = "The constant attribute was removed in Solidity 0.5.0 and has been \
				replaced with stateMutability."
    )]
    /// Constant function.
    constant: bool,
    /// Whether the function reads or modifies blockchain state
    state_mutability: ethabi::StateMutability,
}

impl<'a> From<&'a ethabi::Function> for Function {
    fn from(f: &'a ethabi::Function) -> Self {
        // [param0, hello_world, param2]
        let input_names = input_names(&f.inputs);

        // [T0: Into<Uint>, T1: Into<Bytes>, T2: IntoIterator<Item = U2>, U2 = Into<Uint>]
        let declarations = f
            .inputs
            .iter()
            .enumerate()
            .map(|(index, param)| template_param_type(&param.kind, index));

        // [Uint, Bytes, Vec<Uint>]
        let kinds: Vec<_> = f
            .inputs
            .iter()
            .map(|param| rust_type(&param.kind))
            .collect();

        // [T0, T1, T2]
        let template_names: Vec<_> = get_template_names(&kinds);

        // [param0: T0, hello_world: T1, param2: T2]
        let definitions = input_names
            .iter()
            .zip(template_names.iter())
            .map(|(param_name, template_name)| quote! { #param_name: #template_name });

        let template_params = declarations
            .zip(definitions)
            .map(|(declaration, definition)| TemplateParam {
                declaration,
                definition,
            })
            .collect();

        // [Token::Uint(param0.into()), Token::Bytes(hello_world.into()), Token::Array(param2.into_iter().map(Into::into).collect())]
        let tokenize: Vec<_> = input_names
            .iter()
            .zip(f.inputs.iter())
            .map(|(param_name, param)| {
                to_token(&from_template_param(&param.kind, param_name), &param.kind)
            })
            .collect();

        let output_result = get_output_kinds(&f.outputs);

        let output_implementation = match f.outputs.len() {
            0 => quote! {
                let _output = output;
                Ok(())
            },
            1 => {
                let o = quote! { out };
                let from_first = from_token(&f.outputs[0].kind, &o);
                quote! {
                    let out = self.0.decode_output(output)?.into_iter().next().expect(INTERNAL_ERR);
                    Ok(#from_first)
                }
            }
            _ => {
                let o = quote! { out.next().expect(INTERNAL_ERR) };
                let outs: Vec<_> = f
                    .outputs
                    .iter()
                    .map(|param| from_token(&param.kind, &o))
                    .collect();

                quote! {
                    let mut out = self.0.decode_output(output)?.into_iter();
                    Ok(( #(#outs),* ))
                }
            }
        };

        // The allow deprecated only applies to the field 'constant', but
        // due to this issue: https://github.com/rust-lang/rust/issues/60681
        // it must go on the entire struct
        #[allow(deprecated)]
        Function {
            name: f.name.clone(),
            inputs: Inputs {
                tokenize,
                template_params,
                recreate_quote: to_ethabi_param_vec(&f.inputs),
            },
            outputs: Outputs {
                implementation: output_implementation,
                result: output_result,
                recreate_quote: to_ethabi_param_vec(&f.outputs),
            },
            constant: f.constant.unwrap_or_default(),
            state_mutability: f.state_mutability,
        }
    }
}

impl Function {
    /// Generates the interface for contract's function.
    pub fn generate(&self) -> TokenStream {
        let name = &self.name;
        let module_name = syn::Ident::new(&self.name.to_snake_case(), Span::call_site());
        let tokenize = &self.inputs.tokenize;
        let declarations: &Vec<_> = &self
            .inputs
            .template_params
            .iter()
            .map(|i| &i.declaration)
            .collect();
        let definitions: &Vec<_> = &self
            .inputs
            .template_params
            .iter()
            .map(|i| &i.definition)
            .collect();
        let recreate_inputs = &self.inputs.recreate_quote;
        let recreate_outputs = &self.outputs.recreate_quote;
        #[allow(deprecated)]
        let constant = self.constant;
        let state_mutability = match self.state_mutability {
            ethabi::StateMutability::Pure => quote! { ::ethabi::StateMutability::Pure },
            ethabi::StateMutability::Payable => quote! { ::ethabi::StateMutability::Payable },
            ethabi::StateMutability::NonPayable => quote! { ::ethabi::StateMutability::NonPayable },
            ethabi::StateMutability::View => quote! { ::ethabi::StateMutability::View },
        };
        let outputs_result = &self.outputs.result;
        let outputs_implementation = &self.outputs.implementation;

        quote! {
            pub mod #module_name {
                use ethabi;
                use super::INTERNAL_ERR;

                fn function() -> ethabi::Function {
                    ethabi::Function {
                        name: #name.into(),
                        inputs: #recreate_inputs,
                        outputs: #recreate_outputs,
                        constant: Some(#constant),
                        state_mutability: #state_mutability
                    }
                }

                /// Generic function output decoder.
                pub struct Decoder(ethabi::Function);

                impl ethabi::FunctionOutputDecoder for Decoder {
                    type Output = #outputs_result;

                    fn decode(&self, output: &[u8]) -> ethabi::Result<Self::Output> {
                        #outputs_implementation
                    }
                }

                /// Encodes function input.
                pub fn encode_input<#(#declarations),*>(#(#definitions),*) -> ethabi::Bytes {
                    let f = function();
                    let tokens = vec![#(#tokenize),*];
                    f.encode_input(&tokens).expect(INTERNAL_ERR)
                }

                /// Decodes function output.
                pub fn decode_output(output: &[u8]) -> ethabi::Result<#outputs_result> {
                    ethabi::FunctionOutputDecoder::decode(&Decoder(function()), output)
                }

                /// Encodes function output and creates a `Decoder` instance.
                pub fn call<#(#declarations),*>(#(#definitions),*) -> (ethabi::Bytes, Decoder) {
                    let f = function();
                    let tokens = vec![#(#tokenize),*];
                    (f.encode_input(&tokens).expect(INTERNAL_ERR), Decoder(f))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Function;
    use quote::quote;

    #[test]
    fn test_no_params() {
        #[allow(deprecated)]
        let ethabi_function = ethabi::Function {
            name: "empty".into(),
            inputs: vec![],
            outputs: vec![],
            constant: None,
            state_mutability: ethabi::StateMutability::Payable,
        };

        let f = Function::from(&ethabi_function);

        let expected = quote! {
            pub mod empty {
                use ethabi;
                use super::INTERNAL_ERR;

                fn function() -> ethabi::Function {
                    ethabi::Function {
                        name: "empty".into(),
                        inputs: vec![],
                        outputs: vec![],
                        constant: Some(false),
                        state_mutability: ::ethabi::StateMutability::Payable
                    }
                }

                /// Generic function output decoder.
                pub struct Decoder(ethabi::Function);

                impl ethabi::FunctionOutputDecoder for Decoder {
                    type Output = ();

                    fn decode(&self, output: &[u8]) -> ethabi::Result<Self::Output> {
                        let _output = output;
                        Ok(())
                    }
                }

                /// Encodes function input.
                pub fn encode_input<>() -> ethabi::Bytes {
                    let f = function();
                    let tokens = vec![];
                    f.encode_input(&tokens).expect(INTERNAL_ERR)
                }

                /// Decodes function output.
                pub fn decode_output(output: &[u8]) -> ethabi::Result<()> {
                    ethabi::FunctionOutputDecoder::decode(&Decoder(function()), output)
                }

                /// Encodes function output and creates a `Decoder` instance.
                pub fn call<>() -> (ethabi::Bytes, Decoder) {
                    let f = function();
                    let tokens = vec![];
                    (f.encode_input(&tokens).expect(INTERNAL_ERR), Decoder(f))
                }
            }
        };

        assert_eq!(expected.to_string(), f.generate().to_string());
    }

    #[test]
    fn test_one_param() {
        #[allow(deprecated)]
        let ethabi_function = ethabi::Function {
            name: "hello".into(),
            inputs: vec![ethabi::Param {
                name: "foo".into(),
                kind: ethabi::ParamType::Address,
                internal_type: None,
            }],
            outputs: vec![ethabi::Param {
                name: "bar".into(),
                kind: ethabi::ParamType::Uint(256),
                internal_type: None,
            }],
            constant: None,
            state_mutability: ethabi::StateMutability::Payable,
        };

        let f = Function::from(&ethabi_function);

        let expected = quote! {
            pub mod hello {
                use ethabi;
                use super::INTERNAL_ERR;

                fn function() -> ethabi::Function {
                    ethabi::Function {
                        name: "hello".into(),
                        inputs: vec![ethabi::Param {
                            name: "foo".to_owned(),
                            kind: ethabi::ParamType::Address,
                            internal_type: None
                        }],
                        outputs: vec![ethabi::Param {
                            name: "bar".to_owned(),
                            kind: ethabi::ParamType::Uint(256usize),
                            internal_type: None
                        }],
                        constant: Some(false),
                        state_mutability: ::ethabi::StateMutability::Payable
                    }
                }

                /// Generic function output decoder.
                pub struct Decoder(ethabi::Function);

                impl ethabi::FunctionOutputDecoder for Decoder {
                    type Output = ethabi::Uint;

                    fn decode(&self, output: &[u8]) -> ethabi::Result<Self::Output> {
                        let out = self.0.decode_output(output)?.into_iter().next().expect(INTERNAL_ERR);
                        Ok(out.into_uint().expect(INTERNAL_ERR))
                    }
                }

                /// Encodes function input.
                pub fn encode_input<T0: Into<ethabi::Address> >(foo: T0) -> ethabi::Bytes {
                    let f = function();
                    let tokens = vec![ethabi::Token::Address(foo.into())];
                    f.encode_input(&tokens).expect(INTERNAL_ERR)
                }

                /// Decodes function output.
                pub fn decode_output(output: &[u8]) -> ethabi::Result<ethabi::Uint> {
                    ethabi::FunctionOutputDecoder::decode(&Decoder(function()), output)
                }

                /// Encodes function output and creates a `Decoder` instance.
                pub fn call<T0: Into<ethabi::Address> >(foo: T0) -> (ethabi::Bytes, Decoder) {
                    let f = function();
                    let tokens = vec![ethabi::Token::Address(foo.into())];
                    (f.encode_input(&tokens).expect(INTERNAL_ERR), Decoder(f))
                }
            }
        };

        assert_eq!(expected.to_string(), f.generate().to_string());
    }

    #[test]
    fn test_multiple_params() {
        #[allow(deprecated)]
        let ethabi_function = ethabi::Function {
            name: "multi".into(),
            inputs: vec![
                ethabi::Param {
                    name: "foo".into(),
                    kind: ethabi::ParamType::FixedArray(Box::new(ethabi::ParamType::Address), 2),
                    internal_type: None,
                },
                ethabi::Param {
                    name: "bar".into(),
                    kind: ethabi::ParamType::Array(Box::new(ethabi::ParamType::Uint(256))),
                    internal_type: None,
                },
            ],
            outputs: vec![
                ethabi::Param {
                    name: "".into(),
                    kind: ethabi::ParamType::Uint(256),
                    internal_type: None,
                },
                ethabi::Param {
                    name: "".into(),
                    kind: ethabi::ParamType::String,
                    internal_type: None,
                },
            ],
            constant: None,
            state_mutability: ethabi::StateMutability::Payable,
        };

        let f = Function::from(&ethabi_function);

        let expected = quote! {
            pub mod multi {
                use ethabi;
                use super::INTERNAL_ERR;

                fn function() -> ethabi::Function {
                    ethabi::Function {
                        name: "multi".into(),
                        inputs: vec![ethabi::Param {
                            name: "foo".to_owned(),
                            kind: ethabi::ParamType::FixedArray(Box::new(ethabi::ParamType::Address), 2usize),
                            internal_type: None
                        }, ethabi::Param {
                            name: "bar".to_owned(),
                            kind: ethabi::ParamType::Array(Box::new(ethabi::ParamType::Uint(256usize))),
                            internal_type: None
                        }],
                        outputs: vec![ethabi::Param {
                            name: "".to_owned(),
                            kind: ethabi::ParamType::Uint(256usize),
                            internal_type: None
                        }, ethabi::Param {
                            name: "".to_owned(),
                            kind: ethabi::ParamType::String,
                            internal_type: None
                        }],
                        constant: Some(false),
                        state_mutability: ::ethabi::StateMutability::Payable
                    }
                }

                /// Generic function output decoder.
                pub struct Decoder(ethabi::Function);

                impl ethabi::FunctionOutputDecoder for Decoder {
                    type Output = (ethabi::Uint, String);

                    fn decode(&self, output: &[u8]) -> ethabi::Result<Self::Output> {
                        let mut out = self.0.decode_output(output)?.into_iter();
                        Ok((out.next().expect(INTERNAL_ERR).into_uint().expect(INTERNAL_ERR), out.next().expect(INTERNAL_ERR).into_string().expect(INTERNAL_ERR)))
                    }
                }

                /// Encodes function input.
                pub fn encode_input<T0: Into<[U0; 2usize]>, U0: Into<ethabi::Address>, T1: IntoIterator<Item = U1>, U1: Into<ethabi::Uint> >(foo: T0, bar: T1) -> ethabi::Bytes {
                    let f = function();
                    let tokens = vec![{
                        let v = (Box::new(foo.into()) as Box<[_]>).into_vec().into_iter().map(Into::into).collect::<Vec<_>>().into_iter().map(|inner| ethabi::Token::Address(inner)).collect();
                        ethabi::Token::FixedArray(v)
                    }, {
                        let v = bar.into_iter().map(Into::into).collect::<Vec<_>>().into_iter().map(|inner| ethabi::Token::Uint(inner)).collect();
                        ethabi::Token::Array(v)
                    }];
                    f.encode_input(&tokens).expect(INTERNAL_ERR)
                }

                /// Decodes function output.
                pub fn decode_output(output: &[u8]) -> ethabi::Result<(ethabi::Uint, String)> {
                    ethabi::FunctionOutputDecoder::decode(&Decoder(function()), output)
                }

                /// Encodes function output and creates a `Decoder` instance.
                pub fn call<T0: Into<[U0; 2usize]>, U0: Into<ethabi::Address>, T1: IntoIterator<Item = U1>, U1: Into<ethabi::Uint> >(foo: T0, bar: T1) -> (ethabi::Bytes, Decoder) {
                    let f = function();
                    let tokens = vec![{
                        let v = (Box::new(foo.into()) as Box<[_]>).into_vec().into_iter().map(Into::into).collect::<Vec<_>>().into_iter().map(|inner| ethabi::Token::Address(inner)).collect();
                        ethabi::Token::FixedArray(v)
                    }, {
                        let v = bar.into_iter().map(Into::into).collect::<Vec<_>>().into_iter().map(|inner| ethabi::Token::Uint(inner)).collect();
                        ethabi::Token::Array(v)
                    }];
                    (f.encode_input(&tokens).expect(INTERNAL_ERR), Decoder(f))
                }
            }
        };

        assert_eq!(expected.to_string(), f.generate().to_string());
    }
}
