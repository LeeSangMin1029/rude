impl :: bincode :: Encode for ParsedChunk
{
    fn encode < __E : :: bincode :: enc :: Encoder >
    (& self, encoder : & mut __E) ->core :: result :: Result < (), :: bincode
    :: error :: EncodeError >
    {
        :: bincode :: Encode :: encode(&self.kind, encoder) ?; :: bincode ::
        Encode :: encode(&self.name, encoder) ?; :: bincode :: Encode ::
        encode(&self.file, encoder) ?; :: bincode :: Encode ::
        encode(&self.lines, encoder) ?; :: bincode :: Encode ::
        encode(&self.signature, encoder) ?; :: bincode :: Encode ::
        encode(&self.calls, encoder) ?; :: bincode :: Encode ::
        encode(&self.call_lines, encoder) ?; :: bincode :: Encode ::
        encode(&self.types, encoder) ?; :: bincode :: Encode ::
        encode(&self.imports, encoder) ?; :: bincode :: Encode ::
        encode(&self.string_args, encoder) ?; :: bincode :: Encode ::
        encode(&self.param_flows, encoder) ?; :: bincode :: Encode ::
        encode(&self.param_types, encoder) ?; :: bincode :: Encode ::
        encode(&self.field_types, encoder) ?; :: bincode :: Encode ::
        encode(&self.local_types, encoder) ?; :: bincode :: Encode ::
        encode(&self.let_call_bindings, encoder) ?; :: bincode :: Encode ::
        encode(&self.return_type, encoder) ?; :: bincode :: Encode ::
        encode(&self.field_accesses, encoder) ?; :: bincode :: Encode ::
        encode(&self.enum_variants, encoder) ?; :: bincode :: Encode ::
        encode(&self.is_test, encoder) ?; core :: result :: Result :: Ok(())
    }
}