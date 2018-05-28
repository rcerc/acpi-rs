use super::value::{AmlValue, RegionSpace};
use super::{opcodes, AmlError};
use alloc::{string::ToString, String};
use bit_field::BitField;
use core::str;
use {Acpi, AcpiHandler};

#[derive(Clone)]
pub struct AmlStream<'a> {
    data: &'a [u8],
    offset: u32, // TODO: PkgLength can't be longer than u32, but can a whole AML stream?
}

impl<'a> AmlStream<'a> {
    pub unsafe fn new(data: &'a [u8]) -> AmlStream<'a> {
        AmlStream { data, offset: 0 }
    }

    pub fn peek(&mut self) -> Result<u8, AmlError> {
        if (self.offset + 1) >= self.len() {
            Err(AmlError::EndOfStream)
        } else {
            Ok(self.data[self.offset as usize])
        }
    }

    pub fn next(&mut self) -> Result<u8, AmlError> {
        let byte = self.peek()?;
        self.offset += 1;
        Ok(byte)
    }

    pub fn next_u16(&mut self) -> Result<u16, AmlError> {
        let first_byte = self.next()?;
        let second_byte = self.next()?;
        Ok(first_byte as u16 + ((second_byte as u16) << 8))
    }

    pub fn next_u32(&mut self) -> Result<u32, AmlError> {
        let first_byte = self.next()?;
        let second_byte = self.next()?;
        let third_byte = self.next()?;
        Ok(first_byte as u32 + ((second_byte as u32) << 8) + ((third_byte as u32) << 16))
    }

    pub fn next_u64(&mut self) -> Result<u64, AmlError> {
        let first_byte = self.next()?;
        let second_byte = self.next()?;
        let third_byte = self.next()?;
        let forth_byte = self.next()?;
        Ok(first_byte as u64
            + ((second_byte as u64) << 8)
            + ((third_byte as u64) << 16)
            + ((forth_byte as u64) << 24))
    }

    pub fn backtrack(&mut self, amount: u32) -> Result<(), AmlError> {
        match self.offset.checked_sub(amount) {
            Some(new_offset) => {
                self.offset = new_offset;
                Ok(())
            }

            None => Err(AmlError::BacktrackedFromStart),
        }
    }

    pub fn len(&self) -> u32 {
        self.data.len() as u32
    }

    /// This gets the current offset into the stream
    pub fn offset(&self) -> u32 {
        self.offset
    }
}

pub(crate) struct AmlParser<'s, 'a, 'h, H>
where
    'h: 'a,
    H: AcpiHandler + 'h,
{
    acpi: &'a mut Acpi<'h, H>,
    scope: String,
    stream: AmlStream<'s>,
}

impl<'s, 'a, 'h, H> AmlParser<'s, 'a, 'h, H>
where
    'h: 'a,
    H: AcpiHandler + 'h,
{
    pub(crate) fn parse(
        acpi: &'a mut Acpi<'h, H>,
        scope: &str,
        stream: AmlStream<'s>,
    ) -> Result<(), AmlError> {
        let mut parser = AmlParser {
            acpi,
            scope: String::from(scope),
            stream,
        };

        let end_offset = parser.stream.len() as u32;
        parser.parse_term_list(end_offset)
    }

    fn consume_byte<F>(&mut self, predicate: F) -> Result<u8, AmlError>
    where
        F: Fn(u8) -> bool,
    {
        let byte = self.stream.next()?;

        match predicate(byte) {
            true => Ok(byte),
            false => Err(AmlError::UnexpectedByte(byte)),
        }
    }

    fn match_byte<F>(&mut self, predicate: F) -> Result<bool, AmlError>
    where
        F: Fn(u8) -> bool,
    {
        Ok(predicate(self.stream.peek()?))
    }

    fn consume_opcode(&mut self, opcode: u8) -> Result<(), AmlError> {
        self.consume_byte(matches_byte(opcode))?;
        Ok(())
    }

    fn consume_ext_opcode(&mut self, ext_opcode: u8) -> Result<(), AmlError> {
        self.consume_byte(matches_byte(opcodes::EXT_OPCODE_PREFIX))?;
        self.consume_byte(matches_byte(ext_opcode))?;
        Ok(())
    }

    /// See if the next byte in the stream is the specified opcode, but without advancing the
    /// stream or producing an error if the byte doesn't match.
    fn match_opcode(&mut self, opcode: u8) -> Result<bool, AmlError> {
        self.match_byte(matches_byte(opcode))
    }

    fn match_ext_opcode(&mut self, ext_opcode: u8) -> Result<bool, AmlError> {
        if !self.match_byte(matches_byte(opcodes::EXT_OPCODE_PREFIX))? {
            return Ok(false);
        }

        self.stream.next()?;
        if !self.match_byte(matches_byte(ext_opcode))? {
            self.stream.backtrack(1)?;
            return Ok(false);
        }

        Ok(true)
    }

    /// Try to parse the next part of the stream with the given parsing function. This returns any
    /// `AmlError` as an `Err`, except `AmlError::UnexpectedByte`, to which it will return
    /// `Ok(None)`. A successful parse gives `Ok(Some(...))`. On failure, this also reverts any
    /// changes made to the stream, so it's as if the parsing function never run.
    fn try_parse<T, F>(&mut self, parsing_function: F) -> Result<Option<T>, AmlError>
    where
        F: Fn(&mut Self) -> Result<T, AmlError>,
    {
        let stream = self.stream.clone();

        match parsing_function(self) {
            Ok(result) => Ok(Some(result)),

            /*
             * TODO: What about two separate error types here, one to say "this is not an 'x'"
             * (Return Ok(None)) and
             * one to say "this is an 'x' but I didn't expect this byte!" (Return
             * Err(UnexpectedByte))
             */
            Err(AmlError::UnexpectedByte(_)) => {
                self.stream = stream;
                Ok(None)
            }

            Err(error) => Err(error),
        }
    }

    fn parse_term_list(&mut self, end_offset: u32) -> Result<(), AmlError> {
        /*
         * TermList := Nothing | <TermObj TermList>
         *
         * Because TermLists don't have PkgLengths, we pass the offset to stop at from whatever
         * explicit-length object we were parsing before.
         */

        /*
         * We parse until we reach the offset marked as the end of the structure - `end_offset`
         */
        while self.stream.offset() <= end_offset {
            self.parse_term_object()?;
        }

        Ok(())
    }

    fn parse_term_object(&mut self) -> Result<(), AmlError> {
        trace!("Parsing term object");
        /*
         * TermObj := NameSpaceModifierObj | NamedObj | Type1Opcode | Type2Opcode
         * NameSpaceModifierObj := DefAlias | DefName | DefScope
         * NamedObj := DefBankField | DefCreateBitField | DefCreateByteField | DefCreateDWordField |
         *             DefCreateField | DefCreateQWordField | DefCreateWordField | DefDataRegion |
         *             DefExternal | DefOpRegion | DefPowerRes | DefProcessor | DefThermalZone
         */

        /*
         * TODO: we could get rid of all the matching functions and backtracking entirely and just
         * use `try_parse` for all of these? Probably simplifies parser-complexity-understanding a
         * little?
         */
        if self.match_opcode(opcodes::SCOPE_OP)? {
            return self.parse_def_scope();
        } else if self.match_ext_opcode(opcodes::EXT_OP_REGION_OP)? {
            return self.parse_def_op_region();
        } else if let Some(_) = self.try_parse(AmlParser::parse_def_field)? {
            Ok(())
        } else if let Some(_) = self.try_parse(AmlParser::parse_type1_opcode)? {
            Ok(())
        } else {
            Err(AmlError::UnexpectedByte(self.stream.peek()?))
        }
    }

    fn parse_def_scope(&mut self) -> Result<(), AmlError> {
        /*
         * DefScope := 0x10 PkgLength NameString TermList
         */
        trace!("Parsing scope op");
        self.consume_opcode(opcodes::SCOPE_OP)?;
        let scope_end_offset = self.parse_pkg_length()?;

        let name_string = self.parse_name_string()?;
        let containing_scope = self.scope.clone();

        self.scope = name_string;
        let term_list = self.parse_term_list(scope_end_offset)?;
        self.scope = containing_scope;

        Ok(())
    }

    fn parse_def_op_region(&mut self) -> Result<(), AmlError> {
        /*
         * DefOpRegion := ExtOpPrefix 0x80 NameString RegionSpace RegionOffset RegionLen
         * RegionSpace := ByteData (where 0x00      = SystemMemory
         *                                0x01      = SystemIO
         *                                0x02      = PciConfig
         *                                0x03      = EmbeddedControl
         *                                0x04      = SMBus
         *                                0x05      = SystemCMOS
         *                                0x06      = PciBarTarget
         *                                0x07      = IPMI
         *                                0x08      = GeneralPurposeIO
         *                                0x09      = GenericSerialBus
         *                                0x80-0xff = OEM Defined)
         * ByteData := 0x00 - 0xff
         * RegionOffset := TermArg => Integer
         * RegionLen := TermArg => Integer
         */
        trace!("Parsing def op region");
        self.consume_ext_opcode(opcodes::EXT_OPCODE_PREFIX);

        let name = self.parse_name_string()?;
        info!("name: {}", name);
        let region_space = match self.stream.next()? {
            0x00 => RegionSpace::SystemMemory,
            0x01 => RegionSpace::SystemIo,
            0x02 => RegionSpace::PciConfig,
            0x03 => RegionSpace::EmbeddedControl,
            0x04 => RegionSpace::SMBus,
            0x05 => RegionSpace::SystemCmos,
            0x06 => RegionSpace::PciBarTarget,
            0x07 => RegionSpace::IPMI,
            0x08 => RegionSpace::GeneralPurposeIo,
            0x09 => RegionSpace::GenericSerialBus,
            space @ 0x80..0xff => RegionSpace::OemDefined(space),
            byte => return Err(AmlError::UnexpectedByte(byte)),
        };
        info!("region space: {:?}", region_space);
        let offset = self.parse_term_arg()?.as_integer()?;
        info!("region offset: {}", offset);
        let length = self.parse_term_arg()?.as_integer()?;
        info!("region len: {}", length);

        // Insert it into the namespace
        let namespace_path = self.resolve_path(name)?;
        self.acpi.namespace.insert(
            // self.resolve_path(name)?, TODO: I thought this would work with nll?
            namespace_path,
            AmlValue::OpRegion {
                region: region_space,
                offset,
                length,
            },
        );

        Ok(())
    }

    fn parse_def_field(&mut self) -> Result<(), AmlError> {
        /*
         * DefField = ExtOpPrefix 0x81 PkgLength NameString FieldFlags FieldList
         */
        self.consume_ext_opcode(opcodes::EXT_FIELD_OP)?;
        let end_offset = self.parse_pkg_length()?;
        let name = self.parse_name_string()?;
        let field_flags = self.stream.next()?;
        let field_list = self.parse_field_list(end_offset)?;
        Ok(())
    }

    fn parse_field_list(&mut self, end_offset: u32) -> Result<(), AmlError> {
        /*
         * FieldList := Nothing | <FieldElement FieldList>
         */
        while self.stream.offset() < end_offset {
            self.parse_field_element()?;
        }

        Ok(())
    }

    fn parse_field_element(&mut self) -> Result<(), AmlError> {
        /*
         * FieldElement := NamedField | ReservedField | AccessField | ExtendedAccessField |
         *                 ConnectField
         * NamedField := NameSeg PkgLength
         * ReservedField := 0x00 PkgLength
         * AccessField := 0x01 AccessType AccessAttrib
         * AccessType := ByteData
         * AccessAttrib := ByteData
         * ConnectField := <0x02 NameString> | <0x02 BufferData>
         */
        unimplemented!();
    }

    fn parse_def_buffer(&mut self) -> Result<AmlValue, AmlError> {
        unimplemented!(); // TODO
    }

    fn parse_term_arg(&mut self) -> Result<AmlValue, AmlError> {
        /*
         * TermArg := Type2Opcode | DataObject | ArgObj | LocalObj
         * DataObject := ComputationalData | DefPackage | DefVarPackage
         */
        if let Some(result) = self.try_parse(AmlParser::parse_computational_data)? {
            Ok(result)
        } else {
            Err(AmlError::UnexpectedByte(self.stream.next()?))
        }
    }

    fn parse_computational_data(&mut self) -> Result<AmlValue, AmlError> {
        /*
         * ComputationalData := ByteConst | WordConst | DWordConst | QWordConst | String |
         *                      ConstObj | RevisionOp | DefBuffer
         * ByteConst := 0x0a ByteData
         * WordConst := 0x0b WordData
         * DWordConst := 0x0c DWordData
         * QWordConst := 0x0e QWordData
         * String := 0x0d AsciiCharList NullChar
         * ConstObj := ZeroOp(0x00) | OneOp(0x01) | OnesOp(0xff)
         * RevisionOp := ExtOpPrefix(0x5B) 0x30
         */
        // TODO: can this be rewritten as a cleaner match?
        if self.match_opcode(opcodes::BYTE_CONST)? {
            self.consume_opcode(opcodes::BYTE_CONST)?;
            Ok(AmlValue::Integer(self.stream.next()? as u64))
        } else if self.match_opcode(opcodes::WORD_CONST)? {
            self.consume_opcode(opcodes::WORD_CONST)?;
            Ok(AmlValue::Integer(self.stream.next_u16()? as u64))
        } else if self.match_opcode(opcodes::DWORD_CONST)? {
            self.consume_opcode(opcodes::DWORD_CONST)?;
            Ok(AmlValue::Integer(self.stream.next_u32()? as u64))
        } else if self.match_opcode(opcodes::QWORD_CONST)? {
            self.consume_opcode(opcodes::QWORD_CONST)?;
            Ok(AmlValue::Integer(self.stream.next_u64()? as u64))
        } else if self.match_opcode(opcodes::STRING_PREFIX)? {
            unimplemented!(); // TODO
        } else if self.match_opcode(opcodes::ZERO_OP)? {
            self.stream.next()?;
            Ok(AmlValue::Integer(0))
        } else if self.match_opcode(opcodes::ONE_OP)? {
            self.stream.next()?;
            Ok(AmlValue::Integer(1))
        } else if self.match_opcode(opcodes::ONES_OP)? {
            self.stream.next()?;
            Ok(AmlValue::Integer(u64::max_value()))
        } else if self.match_ext_opcode(opcodes::EXT_REVISION_OP)? {
            unimplemented!(); // TODO
        } else {
            self.parse_def_buffer()
        }
    }

    fn parse_type1_opcode(&mut self) -> Result<(), AmlError> {
        /*
         * Type1Opcode := DefBreak | DefBreakPoint | DefContinue | DefFatal | DefIfElse | DefLoad |
         *                DefNoop | DefNotify | DefRelease | DefReset | DefReturn | DefSignal |
         *                DefSleep | DefStall | DefUnload | DefWhile
         */
        unimplemented!(); // TODO
    }

    /// Parse a PkgLength. Returns the offset into the stream to stop parsing whatever object the
    /// PkgLength describes at.
    fn parse_pkg_length(&mut self) -> Result<u32, AmlError> {
        /*
         * PkgLength := PkgLeadByte |
         *              <PkgLeadByte ByteData> |
         *              <PkgLeadByte ByteData ByteData> |
         *              <PkgLeadByte ByteData ByteData ByteData>
         */
        let lead_byte = self.stream.next()?;
        let byte_data_count = lead_byte.get_bits(6..8);

        if byte_data_count == 0 {
            return Ok(u32::from(lead_byte.get_bits(0..6)));
        }

        let mut length = u32::from(lead_byte.get_bits(0..4));
        for i in 0..byte_data_count {
            length += u32::from(self.stream.next()?) << (4 + i * 8);
        }

        // Minus `byte_data_count + 1` to not include the PkgLength in the remaining bytes
        let end_offset = self.stream.offset() + length - byte_data_count as u32 - 1;
        trace!(
            "Parsed PkgLength with length {}, so ends at {}(current offset={})",
            length,
            end_offset,
            self.stream.offset()
        );
        Ok(end_offset)
    }

    fn parse_name_string(&mut self) -> Result<String, AmlError> {
        /*
         * NameString := <RootChar('\') NamePath> | <PrefixPath NamePath>
         * PrefixPath := Nothing | <'^' PrefixPath>
         */
        match self.stream.peek()? {
            b'\\' => {
                /*
                 * NameString := RootChar NamePath
                 */
                self.stream.next()?;
                Ok(String::from("\\") + &self.parse_name_path()?)
            }

            b'^' => {
                unimplemented!();
            }

            _ => self.parse_name_path(),
        }
    }

    fn parse_name_path(&mut self) -> Result<String, AmlError> {
        /*
         * NamePath := NameSeg | DualNamePath | MultiNamePath | NullPath
         * DualNamePath := DualNamePrefix NameSeg NameSeg
         * MultiNamePath := MultiNamePrefix SegCount{ByteData} NameSeg(..SegCount)
         */
        match self.stream.peek()? {
            opcodes::NULL_NAME => {
                self.stream.next()?;
                Ok(String::from(""))
            }

            opcodes::DUAL_NAME_PREFIX => {
                self.stream.next()?;
                let first = self.parse_name_seg()?;
                let second = self.parse_name_seg()?;

                Ok(
                    String::from(str::from_utf8(&first).unwrap())
                        + str::from_utf8(&second).unwrap(),
                )
            }

            opcodes::MULTI_NAME_PREFIX => {
                // TODO
                unimplemented!();
            }

            _ => Ok(String::from(
                str::from_utf8(&self.parse_name_seg()?).unwrap(),
            )),
        }
    }

    fn parse_name_seg(&mut self) -> Result<[u8; 4], AmlError> {
        /*
         * NameSeg := <LeadNameChar NameChar NameChar NameChar>
         */
        Ok([
            self.consume_byte(is_lead_name_char)?,
            self.consume_byte(is_name_char)?,
            self.consume_byte(is_name_char)?,
            self.consume_byte(is_name_char)?,
        ])
    }

    /// Resolve a given path and the current scope to an absolute path in the namespace.
    fn resolve_path(&mut self, mut path: String) -> Result<String, AmlError> {
        /*
         * TODO: how should we handle '.' as they appear in paths?
         */
        let original_path = path.clone();

        // If the scope to resolve is from the root of the namespace, or the current scope is
        // nothing, just return the given scope
        if self.scope == "" || path.starts_with("\\") {
            return Ok(path);
        }

        // "^"s at the start of a path specify to go up one level from the current scope, to its
        // parent object
        let mut namespace_object = self.scope.clone();
        while path.starts_with("^") {
            path = path[1..].to_string();

            if namespace_object.pop() == None {
                return Err(AmlError::InvalidPath(original_path));
            }
        }

        Ok(namespace_object + &path)
    }
}

fn matches_byte(byte: u8) -> impl Fn(u8) -> bool {
    move |x| x == byte
}

fn is_lead_name_char(byte: u8) -> bool {
    (byte >= b'A' && byte <= b'Z') || byte == b'_'
}

fn is_digit_char(byte: u8) -> bool {
    byte >= b'0' && byte <= b'9'
}

fn is_name_char(byte: u8) -> bool {
    is_lead_name_char(byte) || is_digit_char(byte)
}
