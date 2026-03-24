impl < __Context > :: bincode :: Decode < __Context > for CallGraph
{
    fn decode < __D : :: bincode :: de :: Decoder < Context = __Context > >
    (decoder : & mut __D) ->core :: result :: Result < Self, :: bincode ::
    error :: DecodeError >
    {
        core :: result :: Result ::
        Ok(Self
        {
            version : :: bincode :: Decode :: decode(decoder) ?, names : ::
            bincode :: Decode :: decode(decoder) ?, files : :: bincode ::
            Decode :: decode(decoder) ?, kinds : :: bincode :: Decode ::
            decode(decoder) ?, lines : :: bincode :: Decode :: decode(decoder)
            ?, signatures : :: bincode :: Decode :: decode(decoder) ?,
            name_index : :: bincode :: Decode :: decode(decoder) ?, callees :
            :: bincode :: Decode :: decode(decoder) ?, callers : :: bincode ::
            Decode :: decode(decoder) ?, is_test : :: bincode :: Decode ::
            decode(decoder) ?, trait_impls : :: bincode :: Decode ::
            decode(decoder) ?, impl_of_trait : :: bincode :: Decode ::
            decode(decoder) ?, fn_trait_impl : :: bincode :: Decode ::
            decode(decoder) ?, call_sites : :: bincode :: Decode ::
            decode(decoder) ?, field_access_index : :: bincode :: Decode ::
            decode(decoder) ?,
        })
    }
} impl < '__de, __Context > :: bincode :: BorrowDecode < '__de, __Context >
for CallGraph
{
    fn borrow_decode < __D : :: bincode :: de :: BorrowDecoder < '__de,
    Context = __Context > > (decoder : & mut __D) ->core :: result :: Result <
    Self, :: bincode :: error :: DecodeError >
    {
        core :: result :: Result ::
        Ok(Self
        {
            version : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, names : :: bincode :: BorrowDecode ::<
            '_, __Context >:: borrow_decode(decoder) ?, files : :: bincode ::
            BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?, kinds
            : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, lines : :: bincode :: BorrowDecode ::<
            '_, __Context >:: borrow_decode(decoder) ?, signatures : ::
            bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, name_index : :: bincode :: BorrowDecode
            ::< '_, __Context >:: borrow_decode(decoder) ?, callees : ::
            bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, callers : :: bincode :: BorrowDecode ::<
            '_, __Context >:: borrow_decode(decoder) ?, is_test : :: bincode
            :: BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?,
            trait_impls : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, impl_of_trait : :: bincode ::
            BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?,
            fn_trait_impl : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, call_sites : :: bincode :: BorrowDecode
            ::< '_, __Context >:: borrow_decode(decoder) ?, field_access_index
            : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?,
        })
    }
}