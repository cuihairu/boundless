//! BW-07-02-04: call a cross-process mailbox method.
//!
//! The reference `PyEntityMailBox::callMethod`
//! (`entitydef/mailbox_base.cpp:149`) takes a `MethodDescription` and its
//! arguments, gets a stream on the mailbox's channel (`getStream`), streams
//! the arguments and sends. Each server mailbox kind writes, after the
//! channel's request framing (`startMessage`/`startRequest` +
//! `callBaseMethod`/`callCellMethod`), the same body
//! (`baseapp/mailbox.cpp:755` `getStreamCommon`, `baseapp/mailbox.cpp:1062`
//! `BaseEntityMailBox::getStreamEx`):
//!
//! ```text
//! EntityID   id_                    the mailbox's target entity
//! uint32     internalIndex()        the method index
//! EntityID   sourceEntityID         exposed CELL methods only
//!                                     (method_description.cpp:552)
//! args...    each argument by its schema type
//! ```
//!
//! A method with return values answers on the reply channel
//! (`MethodDescription::sendReturnValues`, `method_description.cpp:970`,
//! framing `startReply(replyID)`), body from `addReturnValues` (line 992):
//!
//! ```text
//! uint8    0        zero for success (line 996)
//! values   by each return value's schema type
//! ```
//!
//! This module is the payload codec over `foundation::stream::BinaryStream`
//! plus the delivery half: `EntityCreator::deliver_mailbox_call` resolves
//! the target in the entity creator, resolves the method on the target's
//! type, checks the arguments and records the call. Script execution is out
//! of the model's scope; the reply a method would send is built by
//! `MethodReply` from the resolved description's return-value kinds. Byte
//! fidelity to the Mercury framing itself is a separate transport concern
//! (the `mercury` crate).

use crate::cell_data::{CellDataError, read_prop_value, write_prop_value};
use crate::entity_creator::{EntityId, MethodComponent, MethodDesc, PropValue};
use foundation::stream::{BinaryStream, StreamError};

/// The failure of a mailbox call or reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailboxCallError {
    /// The mailbox targets a base that does not exist.
    UnknownBase,
    /// The target's type has no method with that internal index.
    UnknownMethod,
    /// The mailbox addresses a cell or client entity, which lives outside
    /// the base-side model this crate tracks.
    CellTarget,
    /// The resolved method lives on a different component than the mailbox
    /// addresses.
    WrongComponent,
    /// The argument count does not match the method's parameter count.
    BadArgs {
        /// The method's parameter count.
        expected: usize,
        /// The streamed argument count.
        got: usize,
    },
    /// An argument's value does not match its declared schema type.
    BadArgKind {
        /// The offending argument index.
        index: usize,
    },
    /// The return-value count does not match the method's return count.
    BadReturnValues {
        /// The method's return count.
        expected: usize,
        /// The supplied count.
        got: usize,
    },
    /// The stream ended early.
    Truncated,
}

impl From<StreamError> for MailboxCallError {
    fn from(_: StreamError) -> Self {
        MailboxCallError::Truncated
    }
}

impl From<CellDataError> for MailboxCallError {
    fn from(_: CellDataError) -> Self {
        MailboxCallError::Truncated
    }
}

/// Whether a value matches the schema type of its witness `kind`.
pub fn value_matches(value: &PropValue, kind: &PropValue) -> bool {
    matches!(
        (value, kind),
        (PropValue::Int(_), PropValue::Int(_))
            | (PropValue::Float(_), PropValue::Float(_))
            | (PropValue::Str(_), PropValue::Str(_))
            | (PropValue::Bool(_), PropValue::Bool(_))
            | (PropValue::Vec3(_), PropValue::Vec3(_))
    )
}

/// The body of a `callBaseMethod` / `callCellMethod` message: the target
/// id, the method index and the streamed arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct MailboxMethodCall {
    /// The target entity id.
    pub target_id: EntityId,
    /// The method's internal index.
    pub method_index: u32,
    /// The source entity id written before the args for an exposed CELL
    /// method, `None` for `NULL_ENTITY_ID`.
    pub source_entity_id: Option<EntityId>,
    /// The streamed arguments (the source entity id excluded).
    pub args: Vec<PropValue>,
}

impl MailboxMethodCall {
    /// Stream a call body after the interface header.
    pub fn write_to(
        &self,
        stream: &mut BinaryStream,
        method: &MethodDesc,
    ) -> Result<(), MailboxCallError> {
        stream.write_u32(self.target_id);
        stream.write_u32(self.method_index);
        if method.component == MethodComponent::Cell && method.exposed {
            stream.write_u32(self.source_entity_id.unwrap_or(0));
        }
        for (value, _kind) in self.args.iter().zip(&method.arg_kinds) {
            write_prop_value(stream, value)?;
        }
        Ok(())
    }

    /// Parse a call body. The method description is needed to know whether
    /// the source id is present and the argument schema.
    pub fn read_from(
        stream: &mut BinaryStream,
        method: &MethodDesc,
    ) -> Result<Self, MailboxCallError> {
        let target_id = stream.read_u32()?;
        let method_index = stream.read_u32()?;
        let source_entity_id = if method.component == MethodComponent::Cell && method.exposed {
            let id = stream.read_u32()?;
            (id != 0).then_some(id)
        } else {
            None
        };
        let mut args = Vec::with_capacity(method.arg_kinds.len());
        for kind in &method.arg_kinds {
            args.push(read_prop_value(stream, kind)?);
        }
        Ok(MailboxMethodCall {
            target_id,
            method_index,
            source_entity_id,
            args,
        })
    }
}

/// The body of a method's reply: a status byte (0 = success) then the
/// return values (`MethodDescription::addReturnValues`,
/// `method_description.cpp:992`).
#[derive(Clone, Debug, PartialEq)]
pub struct MethodReply {
    /// `0` for success, non-zero for a failure response.
    pub status: u8,
    /// The return values.
    pub values: Vec<PropValue>,
}

impl MethodReply {
    /// Build the success reply a method would send, checking the values
    /// against the method's return-value kinds.
    pub fn success(method: &MethodDesc, values: Vec<PropValue>) -> Result<Self, MailboxCallError> {
        if values.len() != method.return_kinds.len() {
            return Err(MailboxCallError::BadReturnValues {
                expected: method.return_kinds.len(),
                got: values.len(),
            });
        }
        for (index, (value, kind)) in values.iter().zip(&method.return_kinds).enumerate() {
            if !value_matches(value, kind) {
                return Err(MailboxCallError::BadArgKind { index });
            }
        }
        Ok(MethodReply { status: 0, values })
    }

    /// Stream a reply body.
    pub fn write_to(
        &self,
        stream: &mut BinaryStream,
        method: &MethodDesc,
    ) -> Result<(), MailboxCallError> {
        stream.write_u8(self.status);
        for (value, _kind) in self.values.iter().zip(&method.return_kinds) {
            write_prop_value(stream, value)?;
        }
        Ok(())
    }

    /// Parse a reply body.
    pub fn read_from(
        stream: &mut BinaryStream,
        method: &MethodDesc,
    ) -> Result<Self, MailboxCallError> {
        let status = stream.read_u8()?;
        let mut values = Vec::with_capacity(method.return_kinds.len());
        for kind in &method.return_kinds {
            values.push(read_prop_value(stream, kind)?);
        }
        Ok(MethodReply { status, values })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base method with a string and an int argument and one int return.
    fn method() -> MethodDesc {
        MethodDesc::new(
            "rename",
            7,
            MethodComponent::Base,
            false,
            vec![PropValue::Str(String::new()), PropValue::Int(0)],
            vec![PropValue::Int(0)],
        )
    }

    /// An exposed cell method: the source id is written before the args.
    fn exposed_cell_method() -> MethodDesc {
        MethodDesc::new(
            "moveTo",
            9,
            MethodComponent::Cell,
            true,
            vec![PropValue::Vec3([0.0; 3]), PropValue::Float(0.0)],
            vec![],
        )
    }

    fn call(method: &MethodDesc) -> MailboxMethodCall {
        MailboxMethodCall {
            target_id: 42,
            method_index: method.internal_index,
            source_entity_id: Some(7),
            args: vec![PropValue::Str("bob".to_string()), PropValue::Int(3)],
        }
    }

    #[test]
    fn a_call_round_trips_with_its_target_and_arguments() {
        let method = method();
        let mut stream = BinaryStream::new();
        call(&method).write_to(&mut stream, &method).expect("write");
        let parsed = MailboxMethodCall::read_from(&mut stream, &method).expect("read");
        assert_eq!(parsed.target_id, 42);
        assert_eq!(parsed.method_index, 7);
        assert_eq!(parsed.source_entity_id, None); // not an exposed cell method
        assert_eq!(
            parsed.args,
            vec![PropValue::Str("bob".to_string()), PropValue::Int(3)]
        );
    }

    #[test]
    fn an_exposed_cell_call_round_trips_with_its_source_id() {
        let method = exposed_cell_method();
        let mut stream = BinaryStream::new();
        let original = MailboxMethodCall {
            target_id: 42,
            method_index: method.internal_index,
            source_entity_id: Some(7),
            args: vec![PropValue::Vec3([1.0, 2.0, 3.0]), PropValue::Float(0.5)],
        };
        original.write_to(&mut stream, &method).expect("write");
        let parsed = MailboxMethodCall::read_from(&mut stream, &method).expect("read");
        assert_eq!(parsed, original);
    }

    #[test]
    fn a_null_source_id_round_trips_as_none() {
        let method = exposed_cell_method();
        let mut stream = BinaryStream::new();
        let original = MailboxMethodCall {
            target_id: 42,
            method_index: method.internal_index,
            source_entity_id: None,
            args: vec![PropValue::Vec3([1.0, 2.0, 3.0]), PropValue::Float(0.5)],
        };
        original.write_to(&mut stream, &method).expect("write");
        let parsed = MailboxMethodCall::read_from(&mut stream, &method).expect("read");
        assert_eq!(parsed.source_entity_id, None);
    }

    #[test]
    fn a_reply_round_trips_with_status_and_values() {
        let method = method();
        let mut stream = BinaryStream::new();
        MethodReply::success(&method, vec![PropValue::Int(9)])
            .expect("success")
            .write_to(&mut stream, &method)
            .expect("write");
        let parsed = MethodReply::read_from(&mut stream, &method).expect("read");
        assert_eq!(parsed.status, 0);
        assert_eq!(parsed.values, vec![PropValue::Int(9)]);
    }

    #[test]
    fn success_rejects_a_wrong_return_count() {
        let method = method();
        assert_eq!(
            MethodReply::success(&method, vec![]),
            Err(MailboxCallError::BadReturnValues {
                expected: 1,
                got: 0
            })
        );
    }

    #[test]
    fn success_rejects_a_wrong_return_type() {
        let method = method();
        assert_eq!(
            MethodReply::success(&method, vec![PropValue::Str("x".to_string())]),
            Err(MailboxCallError::BadArgKind { index: 0 })
        );
    }

    #[test]
    fn a_truncated_call_is_rejected() {
        let method = method();
        let mut stream = BinaryStream::from_slice(&[0, 0, 0]);
        assert_eq!(
            MailboxMethodCall::read_from(&mut stream, &method),
            Err(MailboxCallError::Truncated)
        );
    }

    #[test]
    fn value_matches_tracks_the_schema_variant() {
        assert!(value_matches(&PropValue::Int(1), &PropValue::Int(0)));
        assert!(!value_matches(&PropValue::Int(1), &PropValue::Float(0.0)));
        assert!(!value_matches(
            &PropValue::Vec3([0.0; 3]),
            &PropValue::Bool(true)
        ));
    }
}
