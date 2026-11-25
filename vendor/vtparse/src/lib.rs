//! An implementation of the state machine described by
//! [DEC ANSI Parser](https://vt100.net/emu/dec_ansi_parser), modified to support UTF-8.
//!
//! This is sufficient to broadly categorize ANSI/ECMA-48 escape sequences that are
//! commonly used in terminal emulators.  It does not ascribe semantic meaning to
//! those escape sequences; for example, if you wish to parse the SGR sequence
//! that makes text bold, you will need to know which codes correspond to bold
//! in your implementation of `VTActor`.
//!
//! You may wish to use `termwiz::escape::parser::Parser` in the
//! [termwiz](https://docs.rs/termwiz/) crate if you don't want to have to research
//! all those possible escape sequences for yourself.

#![allow(clippy::upper_case_acronyms)]
#![cfg_attr(not(feature = "std"), no_std)]

use utf8parse::Parser as Utf8Parser;
mod enums;
use crate::enums::*;
mod transitions;

#[cfg(any(feature = "std", feature = "alloc"))]
extern crate alloc;

#[cfg(any(feature = "std", feature = "alloc"))]
use alloc::vec::Vec;
#[cfg(all(not(feature = "std"), not(feature = "alloc")))]
use heapless::Vec;

use transitions::{ENTRY, EXIT, TRANSITIONS};

#[inline(always)]
fn lookup(state: State, b: u8) -> (Action, State) {
    let v = unsafe {
        TRANSITIONS
            .get_unchecked(state as usize)
            .get_unchecked(b as usize)
    };
    (Action::from_u16(v >> 8), State::from_u16(v & 0xff))
}

#[inline(always)]
#[cfg(not(test))]
fn lookup_entry(state: State) -> Action {
    unsafe { *ENTRY.get_unchecked(state as usize) }
}

#[inline(always)]
#[cfg(test)]
fn lookup_entry(state: State) -> Action {
    *ENTRY
        .get(state as usize)
        .unwrap_or_else(|| panic!("State {:?} has no entry in ENTRY", state))
}

#[inline(always)]
#[cfg(test)]
fn lookup_exit(state: State) -> Action {
    *EXIT
        .get(state as usize)
        .unwrap_or_else(|| panic!("State {:?} has no entry in EXIT", state))
}

#[inline(always)]
#[cfg(not(test))]
fn lookup_exit(state: State) -> Action {
    unsafe { *EXIT.get_unchecked(state as usize) }
}

/// `VTActor` is a trait that allows the host application to process
/// the different kinds of sequence as they are parsed from the input
/// stream.
///
/// The functions defined by this trait correspond to the actions defined
/// in the [state machine](https://vt100.net/emu/dec_ansi_parser).
///
/// ## Terminology:
/// An intermediate is a character in the range 0x20-0x2f that
/// occurs before the final character in an escape sequence.
///
/// `ignored_excess_intermediates` is a boolean that is set in the case
/// where there were more than two intermediate characters; no standard
/// defines any codes with more than two.  Intermediates after
/// the second will set this flag and are discarded.
///
/// `params` in most of the functions of this trait are decimal integer parameters in escape
/// sequences.  They are separated by semicolon characters.  An omitted parameter is returned in
/// this interface as a zero, which represents the default value for that parameter.
///
/// Other jargon used here is defined in
/// [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf).
pub trait VTActor {
    /// The current code should be mapped to a glyph according to the character set mappings and
    /// shift states in effect, and that glyph should be displayed.
    ///
    /// If the input was UTF-8 then it will have been mapped to a unicode code point.  Invalid
    /// sequences are represented here using the unicode REPLACEMENT_CHARACTER.
    ///
    /// Otherwise the parameter will be a 7-bit printable value and may be subject to mapping
    /// depending on other state maintained by the embedding application.
    ///
    /// ## Some commentary from the state machine documentation:
    /// GL characters (20 to 7F) are
    /// printed. 20 (SP) and 7F (DEL) are included in this area, although both codes have special
    /// behaviour. If a 94-character set is mapped into GL, 20 will cause a space to be displayed,
    /// and 7F will be ignored. When a 96-character set is mapped into GL, both 20 and 7F may cause
    /// a character to be displayed. Later models of the VT220 included the DEC Multinational
    /// Character Set (MCS), which has 94 characters in its supplemental set (i.e. the characters
    /// supplied in addition to ASCII), so terminals only claiming VT220 compatibility can always
    /// ignore 7F. The VT320 introduced ISO Latin-1, which has 96 characters in its supplemental
    /// set, so emulators with a VT320 compatibility mode need to treat 7F as a printable
    /// character.
    fn print(&mut self, b: char);

    /// The C0 or C1 control function should be executed, which may have any one of a variety of
    /// effects, including changing the cursor position, suspending or resuming communications or
    /// changing the shift states in effect.
    ///
    /// See [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf)
    /// for more information on C0 and C1 control functions.
    fn execute_c0_or_c1(&mut self, control: u8);

    /// invoked when a final character arrives in the first part of a device control string. It
    /// determines the control function from the private marker, intermediate character(s) and
    /// final character, and executes it, passing in the parameter list. It also selects a handler
    /// function for the rest of the characters in the control string.
    ///
    /// See [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf)
    /// for more information on device control strings.
    fn dcs_hook(
        &mut self,
        mode: u8,
        params: &[i64],
        intermediates: &[u8],
        ignored_excess_intermediates: bool,
    );

    /// This action passes characters from the data string part of a device control string to a
    /// handler that has previously been selected by the dcs_hook action. C0 controls are also
    /// passed to the handler.
    ///
    /// See [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf)
    /// for more information on device control strings.
    fn dcs_put(&mut self, byte: u8);

    /// When a device control string is terminated by ST, CAN, SUB or ESC, this action calls the
    /// previously selected handler function with an “end of data” parameter. This allows the
    /// handler to finish neatly.
    ///
    /// See [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf)
    /// for more information on device control strings.
    fn dcs_unhook(&mut self);

    /// Wait for the full seven-bit ST before unhooking a DCS that requires it.
    fn dcs_requires_string_terminator(&self) -> bool {
        false
    }

    /// Identify a completed DCS, including raw C1 ST and the complete ESC form
    /// for actors that request it. The default preserves legacy unhook behavior.
    fn dcs_unhook_with_terminator(&mut self, _terminated: bool) {
        self.dcs_unhook();
    }

    /// The final character of an escape sequence has arrived, so determine the control function
    /// to be executed from the intermediate character(s) and final character, and execute it.
    ///
    /// See [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf)
    /// for more information on escape sequences.
    fn esc_dispatch(
        &mut self,
        params: &[i64],
        intermediates: &[u8],
        ignored_excess_intermediates: bool,
        byte: u8,
    );

    /// A final character of a Control Sequence Initiator has arrived, so determine the control function to be executed from
    /// private marker, intermediate character(s) and final character, and execute it, passing in
    /// the parameter list.
    ///
    /// See [ECMA-48](http://www.ecma-international.org/publications/files/ECMA-ST/ECMA-48,%202nd%20Edition,%20August%201979.pdf)
    /// for more information on control functions.
    fn csi_dispatch(&mut self, params: &[CsiParam], parameters_truncated: bool, byte: u8);

    /// Called when an OSC string is terminated by ST, CAN, SUB or ESC.
    ///
    /// `params` is an array of byte strings (which may also be valid utf-8)
    /// that were passed as semicolon separated parameters to the operating
    /// system command.
    fn osc_dispatch(&mut self, params: &[&[u8]]);

    /// Called when an APC string is terminated by ST
    /// `data` is the data contained within the APC sequence.
    #[cfg(any(feature = "std", feature = "alloc"))]
    fn apc_dispatch(&mut self, data: Vec<u8>);
}

/// `VTAction` is an alternative way to work with the parser; rather
/// than implementing the VTActor trait you can use `CollectingVTActor`
/// to capture the sequence of events into a `Vec<VTAction>`.
#[cfg(any(feature = "std", feature = "alloc"))]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum VTAction {
    Print(char),
    ExecuteC0orC1(u8),
    DcsHook {
        params: Vec<i64>,
        intermediates: Vec<u8>,
        ignored_excess_intermediates: bool,
        byte: u8,
    },
    DcsPut(u8),
    DcsUnhook,
    EscDispatch {
        params: Vec<i64>,
        intermediates: Vec<u8>,
        ignored_excess_intermediates: bool,
        byte: u8,
    },
    CsiDispatch {
        params: Vec<CsiParam>,
        parameters_truncated: bool,
        byte: u8,
    },
    OscDispatch(Vec<Vec<u8>>),
    ApcDispatch(Vec<u8>),
}

/// This is an implementation of `VTActor` that captures the events
/// into an internal vector.
/// It can be iterated via `into_iter` or have the internal
/// vector extracted via `into_vec`.
#[cfg(any(feature = "std", feature = "alloc"))]
#[derive(Default)]
pub struct CollectingVTActor {
    actions: Vec<VTAction>,
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl IntoIterator for CollectingVTActor {
    type Item = VTAction;
    type IntoIter = alloc::vec::IntoIter<VTAction>;

    fn into_iter(self) -> Self::IntoIter {
        self.actions.into_iter()
    }
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl CollectingVTActor {
    pub fn into_vec(self) -> Vec<VTAction> {
        self.actions
    }
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl VTActor for CollectingVTActor {
    fn print(&mut self, b: char) {
        self.actions.push(VTAction::Print(b));
    }

    fn execute_c0_or_c1(&mut self, control: u8) {
        self.actions.push(VTAction::ExecuteC0orC1(control));
    }

    fn dcs_hook(
        &mut self,
        byte: u8,
        params: &[i64],
        intermediates: &[u8],
        ignored_excess_intermediates: bool,
    ) {
        self.actions.push(VTAction::DcsHook {
            byte,
            params: params.to_vec(),
            intermediates: intermediates.to_vec(),
            ignored_excess_intermediates,
        });
    }

    fn dcs_put(&mut self, byte: u8) {
        self.actions.push(VTAction::DcsPut(byte));
    }

    fn dcs_unhook(&mut self) {
        self.actions.push(VTAction::DcsUnhook);
    }

    fn esc_dispatch(
        &mut self,
        params: &[i64],
        intermediates: &[u8],
        ignored_excess_intermediates: bool,
        byte: u8,
    ) {
        self.actions.push(VTAction::EscDispatch {
            params: params.to_vec(),
            intermediates: intermediates.to_vec(),
            ignored_excess_intermediates,
            byte,
        });
    }

    fn csi_dispatch(&mut self, params: &[CsiParam], parameters_truncated: bool, byte: u8) {
        self.actions.push(VTAction::CsiDispatch {
            params: params.to_vec(),
            parameters_truncated,
            byte,
        });
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        self.actions.push(VTAction::OscDispatch(
            params.iter().map(|i| i.to_vec()).collect(),
        ));
    }

    fn apc_dispatch(&mut self, data: Vec<u8>) {
        self.actions.push(VTAction::ApcDispatch(data));
    }
}

const MAX_INTERMEDIATES: usize = 2;
const MAX_OSC: usize = 64;
// Covers a 32,767-UTF-16-unit Windows path even after UTF-8 conversion.
pub const MAX_OSC_DIRECTORY_BYTES: usize = 128 * 1024;
const MAX_OSC_REMOTE_HOST_BYTES: usize = 1024;
// OSC 7501 has a 4096-byte wire limit including its introducer and ST.
const MAX_OSC_PROGRAM_STATUS_BYTES: usize = 4096 - b"\x1b]7501;\x1b\\".len();
// A single OSC 21 can carry all 256 palette entries and named colors.
const MAX_OSC_KITTY_COLORS_BYTES: usize = 16 * 1024;
const MAX_OSC_POINTER_NAMES_BYTES: usize = 4096;
const MAX_OSC_TEXT_SIZE_METADATA: usize = 128;
const MAX_OSC_TEXT_SIZE_BYTES: usize = 4096;
const MAX_NOTIFICATION_METADATA: usize = 1024;
const MAX_NOTIFICATION_WIRE_PAYLOAD: usize = 4096;
pub const MAX_REPORTED_COMMAND_BYTES: usize = 4096;
const MAX_PARAMS: usize = 256;

struct OscState {
    #[cfg(any(feature = "std", feature = "alloc"))]
    buffer: Vec<u8>,
    #[cfg(not(any(feature = "std", feature = "alloc")))]
    buffer: heapless::Vec<u8, { MAX_OSC * 16 }>,
    param_indices: [usize; MAX_OSC],
    num_params: usize,
    full: bool,
    // (selector bytes, maximum literal bytes). Other OSCs keep normal field splitting.
    literal_field: Option<(usize, usize)>,
    // (value offset, discarded). A following parameter is still parsed.
    command_field: Option<(usize, bool)>,
    vscode_limit: Option<usize>,
}

impl OscState {
    fn put(&mut self, param: char) {
        if self.full {
            return;
        }
        if self.vscode_limit.is_some_and(|limit| {
            param.is_control()
                || self.buffer.len() + self.num_params + param.len_utf8() > limit
        }) {
            self.discard_vscode();
            return;
        }
        let notification_metadata = self.num_params == 2
            && self.param_indices[0] == 2
            && self.buffer.starts_with(b"99");
        let text_size_metadata = self.num_params == 2
            && self.param_indices[0] == 2
            && self.buffer.starts_with(b"66");
        if notification_metadata
            && param != ';'
            && (!param.is_ascii_graphic()
                || self.buffer.len() + param.len_utf8() > 2 + MAX_NOTIFICATION_METADATA)
        {
            self.discard_osc();
            return;
        }
        if text_size_metadata
            && param != ';'
            && (!param.is_ascii_graphic()
                || self.buffer.len() + param.len_utf8() > 2 + MAX_OSC_TEXT_SIZE_METADATA)
        {
            self.discard_osc();
            return;
        }
        if param == ';' && self.literal_field.is_none() {
            self.command_field = None;
            if self.num_params == 1 && self.buffer.as_slice() == b"633" {
                self.vscode_limit = Some(256);
            } else if self.num_params == 2 && self.vscode_limit.is_some() {
                self.vscode_limit = Some(match &self.buffer[3..] {
                    b"E" => 64 + 128 + 4 * MAX_REPORTED_COMMAND_BYTES,
                    b"P" => 64 + 4 * MAX_OSC_DIRECTORY_BYTES,
                    _ => 256,
                });
            }
            if self.num_params == 1 && self.buffer.as_slice() == b"7501" {
                self.literal_field = Some((4, MAX_OSC_PROGRAM_STATUS_BYTES));
            }
            if self.num_params == 1 && self.buffer.as_slice() == b"21" {
                self.literal_field = Some((2, MAX_OSC_KITTY_COLORS_BYTES));
            }
            if self.num_params == 1 && self.buffer.as_slice() == b"22" {
                self.literal_field = Some((2, MAX_OSC_POINTER_NAMES_BYTES));
            }
            if notification_metadata {
                self.literal_field = Some((self.buffer.len(), MAX_NOTIFICATION_WIRE_PAYLOAD));
            }
            if text_size_metadata {
                self.literal_field = Some((self.buffer.len(), MAX_OSC_TEXT_SIZE_BYTES));
            }
            if self.num_params == 2 && self.param_indices[0] == 1 && self.buffer.as_slice() == b"99"
            {
                self.literal_field = Some((2, MAX_OSC_DIRECTORY_BYTES));
            }
            match self.num_params {
                MAX_OSC => {
                    self.full = true;
                }
                num => {
                    self.param_indices[num.saturating_sub(1)] = self.buffer.len();
                    self.num_params += 1;
                }
            }
        } else {
            if param == '='
                && self.num_params >= 3
                && self.param_indices[..2] == [3, 4]
                && self.buffer.starts_with(b"133C")
                && &self.buffer[self.param_indices[self.num_params - 2]..] == b"cmdline_url"
            {
                self.command_field = Some((self.buffer.len() + 1, false));
            }
            if let Some((offset, discarded)) = self.command_field {
                if discarded {
                    return;
                }
                if !param.is_ascii_graphic()
                    || self.buffer.len() + 1 > offset + 3 * MAX_REPORTED_COMMAND_BYTES
                {
                    self.discard_command();
                    return;
                }
            }
            if param == '=' && self.num_params == 2 && self.param_indices[0] == 4 {
                let limit = match self.buffer.as_slice() {
                    b"1337CurrentDir" => Some(MAX_OSC_DIRECTORY_BYTES),
                    b"1337RemoteHost" => Some(MAX_OSC_REMOTE_HOST_BYTES),
                    _ => None,
                };
                if let Some(limit) = limit {
                    self.literal_field = Some((self.buffer.len() + 1, limit));
                }
            }
            if let Some((prefix, limit)) = self.literal_field {
                if param.is_control() || self.buffer.len() + param.len_utf8() > prefix + limit {
                    self.discard_literal();
                    return;
                }
            }
            let mut buf = [0u8; 8];
            let extend_result = self
                .buffer
                .extend_from_slice(param.encode_utf8(&mut buf).as_bytes());

            #[cfg(all(not(feature = "std"), not(feature = "alloc")))]
            {
                if extend_result.is_err() {
                    self.discard_command();
                    self.discard_literal();
                    self.full = true;
                    return;
                }
            }

            let _ = extend_result;

            if self.num_params == 0 {
                self.num_params = 1;
            }
        }
    }

    fn discard_command(&mut self) {
        if let Some((offset, discarded)) = &mut self.command_field {
            self.buffer.truncate(*offset);
            *discarded = true;
        }
    }

    fn discard_vscode(&mut self) {
        if self.vscode_limit.is_some() {
            self.discard_osc();
        }
    }

    fn discard_osc(&mut self) {
        self.buffer.clear();
        self.num_params = 0;
        self.full = true;
    }

    fn discard_literal(&mut self) {
        if let Some((prefix, _)) = self.literal_field {
            if self.num_params == 3 && self.param_indices[0] == 2 && self.buffer.starts_with(b"99")
                || self.buffer.get(..prefix) == Some(b"22")
            {
                // Empty notifications and pointer requests can commit state.
                self.discard_osc();
            } else {
                // Dispatch the empty report at ST, never a usable prefix.
                self.buffer.truncate(prefix);
                self.full = true;
            }
        }
    }
}

/// The virtual terminal parser.  It works together with an implementation of `VTActor`.
pub struct VTParser {
    state: State,

    intermediates: [u8; MAX_INTERMEDIATES],
    num_intermediates: usize,
    ignored_excess_intermediates: bool,

    osc: OscState,

    params: [CsiParam; MAX_PARAMS],
    num_params: usize,
    current_param: Option<CsiParam>,
    params_full: bool,
    #[cfg(any(feature = "std", feature = "alloc"))]
    apc_data: Vec<u8>,
    apc_overflow: bool,

    utf8_parser: Utf8Parser,
    utf8_return_state: State,
}

/// Represents a parameter to a CSI-based escaped sequence.
///
/// CSI escapes typically have the form: `CSI 3 m`, but can also
/// bundle multiple values together: `CSI 3 ; 4 m`.  In both
/// of those examples the parameters are simple integer values
/// and latter of which would be expressed as a slice containing
/// `[CsiParam::Integer(3), CsiParam::Integer(4)]`.
///
/// There are some escape sequences that use colons to subdivide and
/// extend the meaning.  For example: `CSI 4:3 m` is a sequence used
/// to denote a curly underline.  That would be represented as:
/// `[CsiParam::ColonList(vec![Some(4), Some(3)])]`.
///
/// Later: reading ECMA 48, CSI is defined as:
/// CSI P ... P  I ... I  F
/// Where P are parameter bytes in the range 0x30-0x3F [0-9:;<=>?]
/// and I are intermediate bytes in the range 0x20-0x2F
/// and F is the final byte in the range 0x40-0x7E
///
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum CsiParam {
    Integer(i64),
    P(u8),
}

impl core::fmt::Debug for CsiParam {
    fn fmt(&self, fmt: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Integer(i) => write!(fmt, "Integer({i})"),
            Self::P(n) => write!(fmt, "P({})", char::from(*n)),
        }
    }
}

impl Default for CsiParam {
    fn default() -> Self {
        Self::Integer(0)
    }
}

impl CsiParam {
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            _ => None,
        }
    }
}

impl core::fmt::Display for CsiParam {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            CsiParam::Integer(v) => {
                write!(f, "{}", v)?;
            }
            CsiParam::P(p) => {
                write!(f, "{}", *p as char)?;
            }
        }
        Ok(())
    }
}

impl VTParser {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let param_indices = [0usize; MAX_OSC];

        Self {
            state: State::Ground,
            utf8_return_state: State::Ground,

            intermediates: [0, 0],
            num_intermediates: 0,
            ignored_excess_intermediates: false,

            osc: OscState {
                buffer: Vec::new(),
                param_indices,
                num_params: 0,
                full: false,
                literal_field: None,
                command_field: None,
                vscode_limit: None,
            },

            params: [CsiParam::default(); MAX_PARAMS],
            num_params: 0,
            params_full: false,
            current_param: None,

            utf8_parser: Utf8Parser::new(),
            #[cfg(any(feature = "std", feature = "alloc"))]
            apc_data: Vec::new(),
            apc_overflow: false,
        }
    }

    /// Returns if the state machine is in the ground state,
    /// i.e. there is no pending state held by the state machine.
    pub fn is_ground(&self) -> bool {
        self.state == State::Ground
    }

    fn as_integer_params(&self) -> [i64; MAX_PARAMS] {
        let mut res = [0i64; MAX_PARAMS];
        let mut i = 0;
        for src in &self.params[0..self.num_params] {
            if let CsiParam::Integer(value) = src {
                res[i] = *value;
            } else if let CsiParam::P(b';') = src {
                i += 1;
            }
        }
        res
    }

    fn finish_param(&mut self) {
        if let Some(val) = self.current_param.take() {
            if self.num_params < MAX_PARAMS {
                self.params[self.num_params] = val;
                self.num_params += 1;
            }
        }
    }

    /// Promote early intermediates to parameters.
    /// This is handle sequences such as DECSET that use `?`
    /// prior to other numeric parameters.
    /// `?` is technically in the intermediate range and shouldn't
    /// appear in the parameter position according to ECMA 48
    fn promote_intermediates_to_params(&mut self) {
        if self.num_intermediates > 0 {
            for &p in &self.intermediates[..self.num_intermediates] {
                if self.num_params >= MAX_PARAMS {
                    self.ignored_excess_intermediates = true;
                    break;
                }
                self.params[self.num_params] = CsiParam::P(p);
                self.num_params += 1;
            }
            self.num_intermediates = 0;
        }
    }

    fn action(&mut self, action: Action, param: u8, actor: &mut dyn VTActor) {
        match action {
            Action::None | Action::Ignore => {}
            Action::Print => actor.print(param as char),
            Action::Execute => actor.execute_c0_or_c1(param),
            Action::Clear => {
                self.num_intermediates = 0;
                self.ignored_excess_intermediates = false;
                self.osc.num_params = 0;
                self.osc.full = false;
                self.osc.literal_field = None;
                self.osc.command_field = None;
                self.osc.vscode_limit = None;
                self.num_params = 0;
                self.params_full = false;
                self.current_param.take();
                #[cfg(any(feature = "std", feature = "alloc"))]
                {
                    self.apc_data.clear();
                    self.apc_data.shrink_to_fit();
                    self.osc.buffer.clear();
                    self.osc.buffer.shrink_to_fit();
                }
            }
            Action::Collect => {
                if self.num_intermediates < MAX_INTERMEDIATES {
                    self.intermediates[self.num_intermediates] = param;
                    self.num_intermediates += 1;
                } else {
                    self.ignored_excess_intermediates = true;
                }
            }
            Action::Param => {
                if self.params_full {
                    return;
                }

                self.promote_intermediates_to_params();

                match param {
                    b'0'..=b'9' => match self.current_param.take() {
                        Some(CsiParam::Integer(i)) => {
                            self.current_param.replace(CsiParam::Integer(
                                i.saturating_mul(10).saturating_add((param - b'0') as i64),
                            ));
                        }
                        Some(_) => unreachable!(),
                        None => {
                            self.current_param
                                .replace(CsiParam::Integer((param - b'0') as i64));
                        }
                    },
                    p => {
                        self.finish_param();

                        if self.num_params + 1 > MAX_PARAMS {
                            self.params_full = true;
                        } else {
                            self.params[self.num_params] = CsiParam::P(p);
                            self.num_params += 1;
                        }
                    }
                }
            }
            Action::Hook => {
                self.finish_param();
                actor.dcs_hook(
                    param,
                    &self.as_integer_params()[0..self.num_params],
                    &self.intermediates[0..self.num_intermediates],
                    self.ignored_excess_intermediates,
                );
            }
            Action::Put => actor.dcs_put(param),
            Action::EscDispatch => {
                self.finish_param();
                actor.esc_dispatch(
                    &self.as_integer_params()[0..self.num_params],
                    &self.intermediates[0..self.num_intermediates],
                    self.ignored_excess_intermediates,
                    param,
                );
            }
            Action::CsiDispatch => {
                self.finish_param();
                self.promote_intermediates_to_params();
                actor.csi_dispatch(
                    &self.params[0..self.num_params],
                    self.ignored_excess_intermediates,
                    param,
                );
            }
            Action::Unhook => actor.dcs_unhook_with_terminator(param == 0x9c),
            Action::OscStart => {
                self.osc.buffer.clear();
                #[cfg(any(feature = "std", feature = "alloc"))]
                self.osc.buffer.shrink_to_fit();
                self.osc.num_params = 0;
                self.osc.full = false;
                self.osc.literal_field = None;
                self.osc.command_field = None;
                self.osc.vscode_limit = None;
            }
            Action::OscPut => self.osc.put(param as char),
            Action::OscIgnore => {
                self.osc.discard_command();
                self.osc.discard_literal();
                self.osc.discard_vscode();
            }

            Action::OscEnd => {
                if !matches!(param, 0x07 | 0x1b | 0x9c) {
                    self.osc.discard_command();
                    self.osc.discard_literal();
                    self.osc.discard_vscode();
                }
                if self.osc.num_params == 0 {
                    actor.osc_dispatch(&[]);
                } else {
                    let mut params: [&[u8]; MAX_OSC] = [b""; MAX_OSC];
                    let mut offset = 0usize;
                    let mut slice = self.osc.buffer.as_slice();
                    let limit = self.osc.num_params.min(MAX_OSC);
                    #[allow(clippy::needless_range_loop)]
                    for i in 0..limit - 1 {
                        let (a, b) = slice.split_at(self.osc.param_indices[i] - offset);
                        params[i] = a;
                        slice = b;
                        offset = self.osc.param_indices[i];
                    }
                    params[limit - 1] = slice;
                    actor.osc_dispatch(&params[0..limit]);
                }
            }

            Action::ApcStart => {
                self.apc_overflow = false;
                #[cfg(any(feature = "std", feature = "alloc"))]
                {
                    self.apc_data.clear();
                    self.apc_data.shrink_to_fit();
                }
            }
            Action::ApcPut => {
                #[cfg(any(feature = "std", feature = "alloc"))]
                {
                    const MAX_APC: usize = 48 * 1024 * 1024;
                    if !self.apc_overflow && self.apc_data.len() < MAX_APC {
                        self.apc_data.push(param);
                    } else if !self.apc_overflow {
                        self.apc_overflow = true;
                        self.apc_data = Vec::new();
                    }
                }
            }
            Action::ApcEnd =>
            {
                #[cfg(any(feature = "std", feature = "alloc"))]
                if !self.apc_overflow {
                    actor.apc_dispatch(core::mem::take(&mut self.apc_data));
                }
            }

            Action::Utf8 => self.next_utf8(actor, param),
        }
    }

    // Process a utf-8 multi-byte sequence.
    // The state tables emit Action::Utf8 to initiate a multi-byte
    // sequence, and once we're in the utf-8 state we'll defer to
    // this method for each byte until the Decode struct is signalled
    // that we're done.
    // We use the REPLACEMENT_CHARACTER for invalid sequences.
    // We return to the ground state after each codepoint, successful
    // or otherwise.
    fn next_utf8(&mut self, actor: &mut dyn VTActor, byte: u8) {
        struct Decoder {
            codepoint: Option<char>,
            invalid: bool,
        }

        impl utf8parse::Receiver for Decoder {
            fn codepoint(&mut self, c: char) {
                self.codepoint.replace(c);
            }

            fn invalid_sequence(&mut self) {
                self.invalid = true;
                self.codepoint(char::REPLACEMENT_CHARACTER);
            }
        }

        let mut decoder = Decoder {
            codepoint: None,
            invalid: false,
        };

        self.utf8_parser.advance(&mut decoder, byte);
        if let Some(c) = decoder.codepoint {
            if decoder.invalid
                && self.utf8_return_state == State::OscString
                && self.osc.literal_field.is_some()
            {
                self.osc.discard_literal();
                self.state = State::OscString;
                if matches!(byte, 0x07 | 0x1b | 0x9c | 0x18 | 0x1a) {
                    self.parse_byte(byte, actor);
                }
                return;
            }
            // Slightly gross special cases C1 controls that were
            // encoded as UTF-8 rather than emitted as raw 8-bit.
            // If the decoded value is in the byte range, and that
            // value would cause a state transition, then we process
            // that state transition rather than performing the default
            // string accumulation.
            if c as u32 <= 0xff {
                let byte = ((c as u32) & 0xff) as u8;

                let (action, state) = lookup(self.utf8_return_state, byte);
                if action == Action::Execute
                    || (state != self.utf8_return_state && state != State::Utf8Sequence)
                {
                    self.action(lookup_exit(self.utf8_return_state), byte, actor);
                    self.action(action, byte, actor);
                    self.action(lookup_entry(state), 0, actor);
                    self.utf8_return_state = self.state;
                    self.state = state;
                    return;
                }
            }

            match self.utf8_return_state {
                State::Ground => actor.print(c),
                State::OscString => self.osc.put(c),
                state => panic!("unreachable state {:?}", state),
            };
            self.state = self.utf8_return_state;
        }
    }

    /// Parse a single byte.  This may result in a call to one of the
    /// methods on the provided `actor`.
    #[inline(always)]
    pub fn parse_byte(&mut self, byte: u8, actor: &mut dyn VTActor) {
        // While in utf-8 parsing mode, co-opt the vt state
        // table and instead use the utf-8 state table from the
        // parser.  It will drop us back into the Ground state
        // after each recognized (or invalid) codepoint.
        match self.state {
            State::Utf8Sequence => {
                self.next_utf8(actor, byte);
                return;
            }
            State::DcsStringEscape => {
                self.action(Action::Unhook, if byte == b'\\' { 0x9c } else { 0x1b }, actor);
                self.state = State::Escape;
                self.action(lookup_entry(State::Escape), 0x1b, actor);
            }
            State::OscLiteralEscape => {
                // The ESC may have arrived at the end of a read. Commit the
                // literal only for ST, then process this byte in Escape as usual.
                if byte != b'\\' {
                    self.osc.discard_literal();
                }
                self.action(Action::OscEnd, 0x1b, actor);
                self.state = State::Escape;
                self.action(lookup_entry(State::Escape), 0x1b, actor);
            }
            _ => {}
        }

        let (action, state) = lookup(self.state, byte);

        if state != self.state {
            if self.state == State::DcsPassthrough
                && state == State::Escape
                && actor.dcs_requires_string_terminator()
            {
                self.state = State::DcsStringEscape;
                return;
            }
            if self.state == State::OscString
                && state == State::Escape
                && self.osc.literal_field.is_some()
            {
                self.state = State::OscLiteralEscape;
                return;
            }
            if state != State::Utf8Sequence {
                self.action(lookup_exit(self.state), byte, actor);
            }
            self.action(action, byte, actor);
            self.action(lookup_entry(state), byte, actor);
            self.utf8_return_state = self.state;
            self.state = state;
        } else {
            self.action(action, byte, actor);
        }
    }

    /// Parse a sequence of bytes.  The sequence need not be complete.
    /// This may result in some number of calls to the methods on the
    /// provided `actor`.
    pub fn parse(&mut self, bytes: &[u8], actor: &mut dyn VTActor) {
        for b in bytes {
            self.parse_byte(*b, actor);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use k9::assert_equal as assert_eq;

    fn parse_as_vec(bytes: &[u8]) -> Vec<VTAction> {
        let mut parser = VTParser::new();
        let mut actor = CollectingVTActor::default();
        parser.parse(bytes, &mut actor);
        actor.into_vec()
    }

    #[test]
    fn test_mixed() {
        assert_eq!(
            parse_as_vec(b"yo\x07\x1b[32mwoot\x1b[0mdone"),
            vec![
                VTAction::Print('y'),
                VTAction::Print('o'),
                VTAction::ExecuteC0orC1(0x07,),
                VTAction::CsiDispatch {
                    params: vec![CsiParam::Integer(32)],
                    parameters_truncated: false,
                    byte: b'm',
                },
                VTAction::Print('w',),
                VTAction::Print('o',),
                VTAction::Print('o',),
                VTAction::Print('t',),
                VTAction::CsiDispatch {
                    params: vec![CsiParam::Integer(0)],
                    parameters_truncated: false,
                    byte: b'm',
                },
                VTAction::Print('d',),
                VTAction::Print('o',),
                VTAction::Print('n',),
                VTAction::Print('e',),
            ]
        );
    }

    #[test]
    fn test_print() {
        assert_eq!(
            parse_as_vec(b"yo"),
            vec![VTAction::Print('y'), VTAction::Print('o')]
        );
    }

    #[test]
    fn test_osc_with_c1_st() {
        assert_eq!(
            parse_as_vec(b"\x1b]0;there\x9c"),
            vec![VTAction::OscDispatch(vec![
                b"0".to_vec(),
                b"there".to_vec()
            ])]
        );
    }

    #[test]
    fn test_osc_with_bel_st() {
        assert_eq!(
            parse_as_vec(b"\x1b]0;hello\x07"),
            vec![VTAction::OscDispatch(vec![
                b"0".to_vec(),
                b"hello".to_vec()
            ])]
        );
    }

    #[test]
    fn iterm_directory_fields_are_literal_bounded_and_scoped() {
        let path = format!("/Pilot's ;#%2F 雪{}", ";".repeat(MAX_OSC + 5));
        let bytes =
            format!("\x1b]1337;CurrentDir={path}\x07\x1b]1337;File=name=YQ==;inline=1:WA==\x07");
        assert_eq!(
            parse_as_vec(bytes.as_bytes()),
            vec![
                VTAction::OscDispatch(vec![
                    b"1337".to_vec(),
                    format!("CurrentDir={path}").into_bytes()
                ]),
                VTAction::OscDispatch(vec![
                    b"1337".to_vec(),
                    b"File=name=YQ==".to_vec(),
                    b"inline=1:WA==".to_vec()
                ]),
            ]
        );

        for (name, limit) in [
            ("CurrentDir", MAX_OSC_DIRECTORY_BYTES),
            ("RemoteHost", MAX_OSC_REMOTE_HOST_BYTES),
        ] {
            let oversized = ";".repeat(limit + 1);
            let bytes = format!("\x1b]1337;{name}={oversized}\x07\x1b]1337;RemoteHost=u@h\x07");
            assert_eq!(
                parse_as_vec(bytes.as_bytes()),
                vec![
                    VTAction::OscDispatch(vec![b"1337".to_vec(), format!("{name}=").into_bytes()]),
                    VTAction::OscDispatch(vec![b"1337".to_vec(), b"RemoteHost=u@h".to_vec()]),
                ]
            );
        }
    }

    #[test]
    fn iterm_directory_invalid_bytes_do_not_select_a_shortened_name() {
        for suffix in [
            &b"a\nb\x07"[..],
            b"a\x7fb\x07",
            b"a\x9db\x07",
            b"a\xffb\x07",
            b"a\xc3!\x07",
            b"a\xc3\x07",
        ] {
            let mut bytes = b"\x1b]1337;CurrentDir=/".to_vec();
            bytes.extend_from_slice(suffix);
            bytes.extend_from_slice(b"OK");
            assert_eq!(
                parse_as_vec(&bytes),
                vec![
                    VTAction::OscDispatch(vec![b"1337".to_vec(), b"CurrentDir=".to_vec()]),
                    VTAction::Print('O'),
                    VTAction::Print('K'),
                ]
            );
        }
    }

    #[test]
    fn literal_directory_requires_a_complete_terminator() {
        let mut parser = VTParser::new();
        let mut actor = CollectingVTActor::default();
        parser.parse(b"\x1b]1337;CurrentDir=/ok\x1b", &mut actor);
        assert!(actor.actions.is_empty());
        parser.parse(b"\\", &mut actor);
        assert_eq!(
            actor.actions.first(),
            Some(&VTAction::OscDispatch(vec![
                b"1337".to_vec(),
                b"CurrentDir=/ok".to_vec()
            ]))
        );
        for ending in [&b"\x1b[0m"[..], b"\x18", b"\x1a"] {
            let mut input = b"\x1b]1337;CurrentDir=/partial".to_vec();
            input.extend_from_slice(ending);
            input.extend_from_slice(b"\x1b]1337;RemoteHost=u@h\x9c");
            let observed: Vec<_> = parse_as_vec(&input)
                .into_iter()
                .filter(|a| matches!(a, VTAction::OscDispatch(_)))
                .collect();
            assert_eq!(
                observed,
                vec![
                    VTAction::OscDispatch(vec![b"1337".to_vec(), b"CurrentDir=".to_vec()]),
                    VTAction::OscDispatch(vec![b"1337".to_vec(), b"RemoteHost=u@h".to_vec()]),
                ]
            );
        }
    }

    #[test]
    fn test_decset() {
        assert_eq!(
            parse_as_vec(b"\x1b[?1l"),
            vec![VTAction::CsiDispatch {
                params: vec![CsiParam::P(b'?'), CsiParam::Integer(1)],
                parameters_truncated: false,
                byte: b'l',
            },]
        );
    }

    #[test]
    fn test_osc_too_many_params() {
        let fields = (0..MAX_OSC + 2)
            .into_iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>();
        let input = format!("\x1b]{}\x07", fields.join(";"));
        let actions = parse_as_vec(input.as_bytes());
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            VTAction::OscDispatch(parsed_fields) => {
                let fields: Vec<_> = fields.into_iter().map(|s| s.as_bytes().to_vec()).collect();
                assert_eq!(parsed_fields.as_slice(), &fields[0..MAX_OSC]);
            }
            other => panic!("Expected OscDispatch but got {:?}", other),
        }
    }

    #[test]
    fn test_osc_with_no_params() {
        assert_eq!(
            parse_as_vec(b"\x1b]\x07"),
            vec![VTAction::OscDispatch(vec![])]
        );
    }

    #[test]
    fn test_osc_with_esc_sequence_st() {
        // This case isn't the same as the other OSC cases; even though
        // `ESC \` is the long form escape sequence for ST, the ESC on its
        // own breaks out of the OSC state and jumps into the ESC state,
        // and that leaves the `\` character to be dispatched there in
        // the calling application.
        assert_eq!(
            parse_as_vec(b"\x1b]woot\x1b\\"),
            vec![
                VTAction::OscDispatch(vec![b"woot".to_vec()]),
                VTAction::EscDispatch {
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                    byte: b'\\'
                }
            ]
        );
    }

    #[test]
    fn test_fancy_underline() {
        assert_eq!(
            parse_as_vec(b"\x1b[4m"),
            vec![VTAction::CsiDispatch {
                params: vec![CsiParam::Integer(4)],
                parameters_truncated: false,
                byte: b'm'
            }]
        );

        assert_eq!(
            // This is the kitty curly underline sequence.
            parse_as_vec(b"\x1b[4:3m"),
            vec![VTAction::CsiDispatch {
                params: vec![
                    CsiParam::Integer(4),
                    CsiParam::P(b':'),
                    CsiParam::Integer(3)
                ],
                parameters_truncated: false,
                byte: b'm'
            }]
        );
    }

    #[test]
    fn test_colon_rgb() {
        assert_eq!(
            parse_as_vec(b"\x1b[38:2::128:64:192m"),
            vec![VTAction::CsiDispatch {
                params: vec![
                    CsiParam::Integer(38),
                    CsiParam::P(b':'),
                    CsiParam::Integer(2),
                    CsiParam::P(b':'),
                    CsiParam::P(b':'),
                    CsiParam::Integer(128),
                    CsiParam::P(b':'),
                    CsiParam::Integer(64),
                    CsiParam::P(b':'),
                    CsiParam::Integer(192),
                ],
                parameters_truncated: false,
                byte: b'm'
            }]
        );
    }

    #[test]
    fn test_csi_omitted_param() {
        assert_eq!(
            parse_as_vec(b"\x1b[;1m"),
            vec![VTAction::CsiDispatch {
                params: vec![CsiParam::P(b';'), CsiParam::Integer(1)],
                parameters_truncated: false,
                byte: b'm'
            }]
        );
    }

    #[test]
    fn test_csi_too_many_params() {
        // Due to the much higher CSI element limit,
        // we must construct this test differently.
        let mut input = "\x1b[0".to_string();
        let mut params = vec![CsiParam::default()];

        for n in 1..=127 {
            input.push_str(&format!(";{n}"));
            params.push(CsiParam::P(b';'));
            params.push(CsiParam::Integer(n));
        }
        input.push_str(";128");

        input.push('p');
        params.push(CsiParam::P(b';'));

        assert_eq!(
            parse_as_vec(input.as_bytes()),
            vec![VTAction::CsiDispatch {
                params: params,
                parameters_truncated: false,
                byte: b'p'
            }]
        );
    }

    #[test]
    fn test_csi_intermediates() {
        assert_eq!(
            parse_as_vec(b"\x1b[1 p"),
            vec![VTAction::CsiDispatch {
                params: vec![CsiParam::Integer(1), CsiParam::P(b' ')],
                parameters_truncated: false,
                byte: b'p'
            }]
        );
        assert_eq!(
            parse_as_vec(b"\x1b[1 !p"),
            vec![VTAction::CsiDispatch {
                params: vec![CsiParam::Integer(1), CsiParam::P(b' '), CsiParam::P(b'!')],
                parameters_truncated: false,
                byte: b'p'
            }]
        );
        assert_eq!(
            parse_as_vec(b"\x1b[1 !#p"),
            vec![VTAction::CsiDispatch {
                // Note that the `#` was discarded
                params: vec![CsiParam::Integer(1), CsiParam::P(b' '), CsiParam::P(b'!')],
                parameters_truncated: true,
                byte: b'p'
            }]
        );
    }

    #[test]
    fn osc_utf8() {
        assert_eq!(
            parse_as_vec("\x1b]\u{af}\x07".as_bytes()),
            vec![VTAction::OscDispatch(vec!["\u{af}".as_bytes().to_vec()])]
        );
    }

    #[test]
    fn osc_fedora_vte() {
        assert_eq!(
            parse_as_vec("\u{9d}777;preexec\u{9c}".as_bytes()),
            vec![VTAction::OscDispatch(vec![
                b"777".to_vec(),
                b"preexec".to_vec(),
            ])]
        );
    }

    #[test]
    fn print_utf8() {
        assert_eq!(
            parse_as_vec("\u{af}".as_bytes()),
            vec![VTAction::Print('\u{af}')]
        );
    }

    #[test]
    fn utf8_control() {
        assert_eq!(
            parse_as_vec("\u{8d}".as_bytes()),
            vec![VTAction::ExecuteC0orC1(0x8d)]
        );
    }

    #[test]
    fn tmux_control() {
        assert_eq!(
            parse_as_vec("\x1bP1000phello\x1b\\".as_bytes()),
            vec![
                VTAction::DcsHook {
                    byte: b'p',
                    params: vec![1000],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                },
                VTAction::DcsPut(b'h'),
                VTAction::DcsPut(b'e'),
                VTAction::DcsPut(b'l'),
                VTAction::DcsPut(b'l'),
                VTAction::DcsPut(b'o'),
                VTAction::DcsUnhook,
                VTAction::EscDispatch {
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                    byte: b'\\',
                }
            ]
        );
    }

    #[test]
    fn tmux_passthru() {
        // I'm not convinced that we *should* represent this tmux sequence
        // in this way, but it is how it currently maps.
        // It's worth noting that we see this as final byte `t` here, which
        // collides with decVT105G in https://vt100.net/emu/dcsseq_dec.html
        assert_eq!(
            parse_as_vec("\x1bPtmux;data\x1b\\".as_bytes()),
            vec![
                VTAction::DcsHook {
                    byte: b't',
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                },
                VTAction::DcsPut(b'm'),
                VTAction::DcsPut(b'u'),
                VTAction::DcsPut(b'x'),
                VTAction::DcsPut(b';'),
                VTAction::DcsPut(b'd'),
                VTAction::DcsPut(b'a'),
                VTAction::DcsPut(b't'),
                VTAction::DcsPut(b'a'),
                VTAction::DcsUnhook,
                VTAction::EscDispatch {
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                    byte: b'\\',
                }
            ]
        );
    }

    #[test]
    fn kitty_img() {
        assert_eq!(
            parse_as_vec("\x1b_Gf=24,s=10,v=20;payload\x1b\\".as_bytes()),
            vec![
                VTAction::ApcDispatch(b"Gf=24,s=10,v=20;payload".to_vec()),
                VTAction::EscDispatch {
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                    byte: b'\\',
                }
            ]
        );
    }

    #[test]
    fn sixel() {
        assert_eq!(
            parse_as_vec("\x1bPqhello\x1b\\".as_bytes()),
            vec![
                VTAction::DcsHook {
                    byte: b'q',
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                },
                VTAction::DcsPut(b'h'),
                VTAction::DcsPut(b'e'),
                VTAction::DcsPut(b'l'),
                VTAction::DcsPut(b'l'),
                VTAction::DcsPut(b'o'),
                VTAction::DcsUnhook,
                VTAction::EscDispatch {
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                    byte: b'\\',
                }
            ]
        );
    }

    #[test]
    fn test_ommitted_dcs_param() {
        assert_eq!(
            parse_as_vec("\x1bP;1q\x1b\\".as_bytes()),
            vec![
                VTAction::DcsHook {
                    byte: b'q',
                    params: vec![0, 1],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                },
                VTAction::DcsUnhook,
                VTAction::EscDispatch {
                    params: vec![],
                    intermediates: vec![],
                    ignored_excess_intermediates: false,
                    byte: b'\\',
                }
            ]
        );
    }
}
