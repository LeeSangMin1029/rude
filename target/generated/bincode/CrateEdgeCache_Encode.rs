impl :: bincode :: Encode for CrateEdgeCache
{
    fn encode < __E : :: bincode :: enc :: Encoder >
    (& self, encoder : & mut __E) ->core :: result :: Result < (), :: bincode
    :: error :: EncodeError >
    {
        :: bincode :: Encode :: encode(&self.idx_edges, encoder) ?; core ::
        result :: Result :: Ok(())
    }
}