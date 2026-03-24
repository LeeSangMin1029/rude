impl :: bincode :: Encode for EdgeCacheBundle
{
    fn encode < __E : :: bincode :: enc :: Encoder >
    (& self, encoder : & mut __E) ->core :: result :: Result < (), :: bincode
    :: error :: EncodeError >
    {
        :: bincode :: Encode :: encode(&self.chunks_hash, encoder) ?; ::
        bincode :: Encode :: encode(&self.crates, encoder) ?; core :: result
        :: Result :: Ok(())
    }
}