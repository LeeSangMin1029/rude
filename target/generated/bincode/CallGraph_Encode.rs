impl :: bincode :: Encode for CallGraph
{
    fn encode < __E : :: bincode :: enc :: Encoder >
    (& self, encoder : & mut __E) ->core :: result :: Result < (), :: bincode
    :: error :: EncodeError >
    {
        :: bincode :: Encode :: encode(&self.version, encoder) ?; :: bincode
        :: Encode :: encode(&self.names, encoder) ?; :: bincode :: Encode ::
        encode(&self.files, encoder) ?; :: bincode :: Encode ::
        encode(&self.kinds, encoder) ?; :: bincode :: Encode ::
        encode(&self.lines, encoder) ?; :: bincode :: Encode ::
        encode(&self.signatures, encoder) ?; :: bincode :: Encode ::
        encode(&self.name_index, encoder) ?; :: bincode :: Encode ::
        encode(&self.callees, encoder) ?; :: bincode :: Encode ::
        encode(&self.callers, encoder) ?; :: bincode :: Encode ::
        encode(&self.is_test, encoder) ?; :: bincode :: Encode ::
        encode(&self.trait_impls, encoder) ?; :: bincode :: Encode ::
        encode(&self.impl_of_trait, encoder) ?; :: bincode :: Encode ::
        encode(&self.fn_trait_impl, encoder) ?; :: bincode :: Encode ::
        encode(&self.call_sites, encoder) ?; :: bincode :: Encode ::
        encode(&self.field_access_index, encoder) ?; core :: result :: Result
        :: Ok(())
    }
}