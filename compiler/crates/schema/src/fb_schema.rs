/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use crate::definitions::{Argument, Directive, *};
use crate::graphqlschema_generated::graphqlschema::*;
use common::Span;
use flatbuffers::*;
use graphql_syntax::{
    BooleanNode, ConstantArgument, ConstantValue, DirectiveLocation, EnumNode, FloatNode,
    FloatValue, Identifier, IntNode, List, StringNode, Token, TokenKind,
};
use interner::{Intern, StringKey};
use std::convert::TryInto;

#[derive(Debug)]
pub struct FlatBufferSchema<'fb> {
    fb_schema: FBSchema<'fb>,
    types: Vector<'fb, ForwardsUOffset<FBTypeMap<'fb>>>,
    directives: Vector<'fb, ForwardsUOffset<FBDirectiveMap<'fb>>>,
    schema: Schema,
}

impl<'fb> FlatBufferSchema<'fb> {
    pub fn build(bytes: &'fb [u8], schema: Schema) -> Self {
        let fb_schema: FBSchema<'fb> = get_root_as_fbschema(bytes);
        Self {
            fb_schema,
            types: fb_schema.types().unwrap(),
            schema,
            directives: fb_schema.directives().unwrap(),
        }
    }

    pub fn get_type(&mut self, type_name: StringKey) -> Option<Type> {
        if !self.schema.has_type(type_name) {
            return self.read_type(type_name);
        }
        self.schema.get_type(type_name)
    }

    pub fn has_type(&mut self, type_name: StringKey) -> bool {
        self.get_type(type_name).is_some()
    }

    pub fn get_directive(&mut self, directive_name: StringKey) -> Option<&Directive> {
        if self.schema.get_directive(directive_name).is_none() {
            return self.read_directive(directive_name);
        };
        self.schema.get_directive(directive_name)
    }

    pub fn snapshot_print(self) -> String {
        self.schema.snapshot_print()
    }

    fn read_directive(&mut self, name: StringKey) -> Option<&Directive> {
        let mut start = 0;
        let mut end = self.directives.len();
        while start <= end {
            let mid = (start + end) / 2;
            let cmp = self
                .directives
                .get(mid)
                .key_compare_with_value(name.lookup());
            if cmp == ::std::cmp::Ordering::Equal {
                let directive = self.directives.get(mid).value()?;
                return Some(self.parse_directive(directive)?);
            } else if cmp == ::std::cmp::Ordering::Less {
                start = mid + 1;
            } else {
                end = mid - 1;
            }
        }
        None
    }

    fn parse_directive(&mut self, directive: FBDirective<'fb>) -> Option<&Directive> {
        let locations = directive
            .locations()?
            .iter()
            .map(get_mapped_location)
            .collect::<Vec<_>>();
        let parsed_directive = Directive {
            name: directive.name()?.intern(),
            is_extension: directive.is_extension(),
            arguments: self.parse_arguments(directive.arguments()?)?,
            locations,
            repeatable: directive.repeatable(),
        };
        self.schema.add_directive(parsed_directive).unwrap();
        self.schema.get_directive(directive.name()?.intern())
    }

    fn read_type(&mut self, type_name: StringKey) -> Option<Type> {
        let mut start = 0;
        let mut end = self.types.len();
        while start <= end {
            let mid = (start + end) / 2;
            let cmp = self
                .types
                .get(mid)
                .key_compare_with_value(type_name.lookup());
            if cmp == ::std::cmp::Ordering::Equal {
                let type_ = self.types.get(mid).value()?;
                return Some(self.parse_type(type_)?);
            } else if cmp == ::std::cmp::Ordering::Less {
                start = mid + 1;
            } else {
                end = mid - 1;
            }
        }
        None
    }

    fn parse_type(&mut self, type_: FBType<'_>) -> Option<Type> {
        Some(match type_.kind() {
            FBTypeKind::Scalar => self.parse_scalar(type_.scalar_id())?,
            FBTypeKind::InputObject => self.parse_input_object(type_.input_object_id())?,
            FBTypeKind::Enum => self.parse_enum(type_.enum_id())?,
            FBTypeKind::Object => self.parse_object(type_.object_id())?,
            FBTypeKind::Interface => self.parse_interface(type_.interface_id())?,
            FBTypeKind::Union => self.parse_union(type_.union_id())?,
        })
    }

    fn parse_scalar(&mut self, id: u32) -> Option<Type> {
        let scalar = self.fb_schema.scalars()?.get(id.try_into().unwrap());
        let parsed_scalar = Scalar {
            name: scalar.name()?.to_string().intern(),
            is_extension: scalar.is_extension(),
            directives: self.parse_directive_values(scalar.directives()?)?,
        };
        Some(Type::Scalar(self.schema.add_scalar(parsed_scalar).unwrap()))
    }

    fn parse_input_object(&mut self, id: u32) -> Option<Type> {
        let input_object = self.fb_schema.input_objects()?.get(id.try_into().unwrap());
        let parsed_input_object = InputObject {
            name: input_object.name()?.to_string().intern(),
            fields: self.parse_arguments(input_object.fields()?)?,
            directives: self.parse_directive_values(input_object.directives()?)?,
        };
        Some(Type::InputObject(
            self.schema.add_input_object(parsed_input_object).unwrap(),
        ))
    }

    fn parse_enum(&mut self, id: u32) -> Option<Type> {
        let enum_ = self.fb_schema.enums()?.get(id.try_into().unwrap());
        let parsed_enum = Enum {
            name: enum_.name()?.to_string().intern(),
            is_extension: enum_.is_extension(),
            values: self.parse_enum_values(enum_.values()?)?,
            directives: self.parse_directive_values(enum_.directives()?)?,
        };
        Some(Type::Enum(self.schema.add_enum(parsed_enum).unwrap()))
    }

    fn parse_object(&mut self, id: u32) -> Option<Type> {
        let object = self.fb_schema.objects()?.get(id.try_into().unwrap());
        let parsed_object = Object {
            name: object.name()?.intern(),
            is_extension: object.is_extension(),
            fields: vec![],
            interfaces: vec![],
            directives: self.parse_directive_values(object.directives()?)?,
        };
        let new_id = self.schema.add_object(parsed_object).unwrap();
        for field_id in object.fields()? {
            let field = self.parse_field(field_id)?;
            self.schema.add_field_to_object(new_id, field).unwrap();
        }
        for interface_id in object.interfaces()? {
            let interface = self.get_type(
                self.fb_schema
                    .interfaces()?
                    .get(interface_id.try_into().unwrap())
                    .name()?
                    .intern(),
            )?;
            self.schema
                .add_interface_to_object(new_id, interface.get_interface_id()?)
                .unwrap();
        }
        Some(Type::Object(new_id))
    }

    fn parse_interface(&mut self, id: u32) -> Option<Type> {
        let interface = self.fb_schema.interfaces()?.get(id.try_into().unwrap());
        let parsed_interface = Interface {
            name: interface.name()?.intern(),
            is_extension: interface.is_extension(),
            implementing_objects: vec![],
            fields: vec![],
            directives: self.parse_directive_values(interface.directives()?)?,
            interfaces: vec![],
        };
        let new_id = self.schema.add_interface(parsed_interface).unwrap();
        for field_id in interface.fields()? {
            let field = self.parse_field(field_id)?;
            self.schema.add_field_to_interface(new_id, field).unwrap();
        }
        for interface_id in interface.interfaces()? {
            let interface = self.get_type(
                self.fb_schema
                    .interfaces()?
                    .get(interface_id.try_into().unwrap())
                    .name()?
                    .intern(),
            )?;
            self.schema
                .add_parent_interface_to_interface(new_id, interface.get_interface_id()?)
                .unwrap();
        }
        for object_id in interface.implementing_objects()? {
            let object = self.get_type(
                self.fb_schema
                    .objects()?
                    .get(object_id.try_into().unwrap())
                    .name()?
                    .intern(),
            )?;
            self.schema
                .add_implementing_object_to_interface(new_id, object.get_object_id()?)
                .unwrap();
        }
        Some(Type::Interface(new_id))
    }

    fn parse_union(&mut self, id: u32) -> Option<Type> {
        let union = self.fb_schema.unions()?.get(id.try_into().unwrap());
        let parsed_union = Union {
            name: union.name()?.intern(),
            is_extension: union.is_extension(),
            members: vec![],
            directives: self.parse_directive_values(union.directives()?)?,
        };
        let new_id = self.schema.add_union(parsed_union).unwrap();
        for object_id in union.members()? {
            let object = self.get_type(
                self.fb_schema
                    .objects()?
                    .get(object_id.try_into().unwrap())
                    .name()?
                    .intern(),
            )?;
            self.schema
                .add_member_to_union(new_id, object.get_object_id()?)
                .unwrap();
        }
        Some(Type::Union(new_id))
    }

    fn parse_field(&mut self, id: u32) -> Option<FieldID> {
        let field = self.fb_schema.fields()?.get(id.try_into().unwrap());
        let parsed_field = Field {
            name: field.name()?.intern(),
            is_extension: field.is_extension(),
            arguments: self.parse_arguments(field.arguments()?)?,
            type_: self.parse_type_reference(field.type_()?)?,
            directives: self.parse_directive_values(field.directives()?)?,
            parent_type: self.get_type(self.get_fbtype_name(&field.parent_type()?)),
        };
        Some(self.schema.add_field(parsed_field).unwrap())
    }

    fn parse_enum_values(
        &self,
        values: Vector<'_, ForwardsUOffset<FBEnumValue<'_>>>,
    ) -> Option<Vec<EnumValue>> {
        values
            .iter()
            .map(|value| self.parse_enum_value(value))
            .collect::<Option<Vec<_>>>()
    }

    fn parse_enum_value(&self, value: FBEnumValue<'fb>) -> Option<EnumValue> {
        let directives = self.parse_directive_values(value.directives()?)?;
        Some(EnumValue {
            value: value.value()?.intern(),
            directives,
        })
    }

    fn parse_arguments(
        &mut self,
        arguments: Vector<'fb, ForwardsUOffset<FBArgument<'_>>>,
    ) -> Option<ArgumentDefinitions> {
        let items = arguments
            .iter()
            .map(|argument| self.parse_argument(argument))
            .collect::<Option<Vec<_>>>();
        Some(ArgumentDefinitions::new(items?))
    }

    fn parse_argument(&mut self, argument: FBArgument<'fb>) -> Option<Argument> {
        Some(Argument {
            name: argument.name().unwrap().intern(),
            default_value: match argument.value() {
                Some(value) => Some(self.parse_const_value(value)?),
                _ => None,
            },
            type_: self.parse_type_reference(argument.type_()?)?,
        })
    }

    fn parse_type_reference(
        &mut self,
        type_reference: FBTypeReference<'fb>,
    ) -> Option<TypeReference> {
        Some(match type_reference.kind() {
            FBTypeReferenceKind::Named => {
                let type_name = self.get_fbtype_name(&type_reference.named()?);
                TypeReference::Named(self.get_type(type_name).unwrap())
            }
            FBTypeReferenceKind::NonNull => {
                TypeReference::NonNull(Box::new(self.parse_type_reference(type_reference.null()?)?))
            }
            FBTypeReferenceKind::List => {
                TypeReference::List(Box::new(self.parse_type_reference(type_reference.list()?)?))
            }
        })
    }

    fn parse_directive_values(
        &self,
        directives: Vector<'_, ForwardsUOffset<FBDirectiveValue<'_>>>,
    ) -> Option<Vec<DirectiveValue>> {
        directives
            .iter()
            .map(|directive| self.parse_directive_value(directive))
            .collect::<Option<Vec<_>>>()
    }

    fn parse_directive_value(&self, directive: FBDirectiveValue<'fb>) -> Option<DirectiveValue> {
        let arguments = self.parse_argument_values(directive.arguments()?)?;
        Some(DirectiveValue {
            name: directive.name()?.intern(),
            arguments,
        })
    }

    fn parse_argument_values(
        &self,
        arguments: Vector<'_, ForwardsUOffset<FBArgumentValue<'_>>>,
    ) -> Option<Vec<ArgumentValue>> {
        arguments
            .iter()
            .map(|argument| self.parse_argument_value(argument))
            .collect::<Option<Vec<_>>>()
    }

    fn parse_argument_value(&self, argument: FBArgumentValue<'fb>) -> Option<ArgumentValue> {
        Some(ArgumentValue {
            name: argument.name()?.intern(),
            value: self.parse_const_value(argument.value()?)?,
        })
    }

    fn parse_const_value(&self, value: FBConstValue<'fb>) -> Option<ConstantValue> {
        Some(match value.kind() {
            FBConstValueKind::Null => ConstantValue::Null(get_empty_token()),
            FBConstValueKind::String => {
                ConstantValue::String(get_string_node(value.string_value()?.to_string()))
            }
            FBConstValueKind::Bool => ConstantValue::Boolean(get_boolean_node(value.bool_value())),
            FBConstValueKind::Int => {
                ConstantValue::Int(get_int_node(value.int_value()?.to_string()))
            }
            FBConstValueKind::Float => {
                ConstantValue::Float(get_float_node(value.float_value()?.to_string()))
            }
            FBConstValueKind::Enum => {
                ConstantValue::Enum(get_enum_node(value.enum_value()?.to_string()))
            }
            FBConstValueKind::List => {
                ConstantValue::List(self.parse_list_value(value.list_value()?)?)
            }
            FBConstValueKind::Object => {
                ConstantValue::Object(self.parse_object_value(value.object_value()?)?)
            }
        })
    }

    fn parse_list_value(&self, list_value: FBListValue<'fb>) -> Option<List<ConstantValue>> {
        let items = list_value
            .values()?
            .iter()
            .map(|value| self.parse_const_value(value))
            .collect::<Option<Vec<_>>>();
        Some(List {
            span: get_empty_span(),
            start: get_empty_token(),
            items: items?,
            end: get_empty_token(),
        })
    }

    fn parse_object_value(
        &self,
        object_value: FBObjectValue<'fb>,
    ) -> Option<List<ConstantArgument>> {
        let items = object_value
            .fields()?
            .iter()
            .map(|field| {
                Some(ConstantArgument {
                    span: get_empty_span(),
                    name: get_identifier(field.name()?.to_string()),
                    colon: get_empty_token(),
                    value: self.parse_const_value(field.value()?)?,
                })
            })
            .collect::<Option<Vec<_>>>();
        Some(List {
            span: get_empty_span(),
            start: get_empty_token(),
            items: items?,
            end: get_empty_token(),
        })
    }

    fn get_fbtype_name(&self, type_: &FBType<'_>) -> StringKey {
        match type_.kind() {
            FBTypeKind::Scalar => self
                .fb_schema
                .scalars()
                .unwrap()
                .get(type_.scalar_id().try_into().unwrap())
                .name(),
            FBTypeKind::InputObject => self
                .fb_schema
                .input_objects()
                .unwrap()
                .get(type_.input_object_id().try_into().unwrap())
                .name(),
            FBTypeKind::Enum => self
                .fb_schema
                .enums()
                .unwrap()
                .get(type_.enum_id().try_into().unwrap())
                .name(),
            FBTypeKind::Object => self
                .fb_schema
                .objects()
                .unwrap()
                .get(type_.object_id().try_into().unwrap())
                .name(),
            FBTypeKind::Interface => self
                .fb_schema
                .interfaces()
                .unwrap()
                .get(type_.interface_id().try_into().unwrap())
                .name(),
            FBTypeKind::Union => self
                .fb_schema
                .unions()
                .unwrap()
                .get(type_.union_id().try_into().unwrap())
                .name(),
        }
        .unwrap()
        .intern()
    }
}

fn get_identifier(value: String) -> Identifier {
    Identifier {
        span: get_empty_span(),
        token: get_empty_token(),
        value: value.intern(),
    }
}

fn get_enum_node(value: String) -> EnumNode {
    EnumNode {
        token: get_empty_token(),
        value: value.intern(),
    }
}

fn get_float_node(value: String) -> FloatNode {
    FloatNode {
        token: get_empty_token(),
        value: FloatValue::new(value.parse::<f64>().unwrap()),
        source_value: value.intern(),
    }
}

fn get_int_node(value: String) -> IntNode {
    IntNode {
        token: get_empty_token(),
        value: value.parse().unwrap(),
    }
}

fn get_boolean_node(value: bool) -> BooleanNode {
    BooleanNode {
        token: get_empty_token(),
        value,
    }
}

fn get_string_node(value: String) -> StringNode {
    StringNode {
        token: get_empty_token(),
        value: value.intern(),
    }
}

fn get_empty_token() -> Token {
    Token {
        span: get_empty_span(),
        kind: TokenKind::EndOfFile,
    }
}

fn get_empty_span() -> Span {
    Span { start: 0, end: 0 }
}

fn get_mapped_location(location: FBDirectiveLocation) -> DirectiveLocation {
    match location {
        FBDirectiveLocation::Query => DirectiveLocation::Query,
        FBDirectiveLocation::Mutation => DirectiveLocation::Mutation,
        FBDirectiveLocation::Subscription => DirectiveLocation::Subscription,
        FBDirectiveLocation::Field => DirectiveLocation::Field,
        FBDirectiveLocation::FragmentDefinition => DirectiveLocation::FragmentDefinition,
        FBDirectiveLocation::FragmentSpread => DirectiveLocation::FragmentSpread,
        FBDirectiveLocation::InlineFragment => DirectiveLocation::InlineFragment,
        FBDirectiveLocation::Schema => DirectiveLocation::Schema,
        FBDirectiveLocation::Scalar => DirectiveLocation::Scalar,
        FBDirectiveLocation::Object => DirectiveLocation::Object,
        FBDirectiveLocation::FieldDefinition => DirectiveLocation::FieldDefinition,
        FBDirectiveLocation::ArgumentDefinition => DirectiveLocation::ArgumentDefinition,
        FBDirectiveLocation::Interface => DirectiveLocation::Interface,
        FBDirectiveLocation::Union => DirectiveLocation::Union,
        FBDirectiveLocation::Enum => DirectiveLocation::Enum,
        FBDirectiveLocation::EnumValue => DirectiveLocation::EnumValue,
        FBDirectiveLocation::InputObject => DirectiveLocation::InputObject,
        FBDirectiveLocation::InputFieldDefinition => DirectiveLocation::InputFieldDefinition,
        FBDirectiveLocation::VariableDefinition => DirectiveLocation::VariableDefinition,
    }
}