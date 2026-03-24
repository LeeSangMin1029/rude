impl < __Context > :: bincode :: Decode < __Context > for ParsedChunk
{
    fn decode < __D : :: bincode :: de :: Decoder < Context = __Context > >
    (decoder : & mut __D) ->core :: result :: Result < Self, :: bincode ::
    error :: DecodeError >
    {
        core :: result :: Result ::
        Ok(Self
        {
            kind : :: bincode :: Decode :: decode(decoder) ?, name : ::
            bincode :: Decode :: decode(decoder) ?, file : :: bincode ::
            Decode :: decode(decoder) ?, lines : :: bincode :: Decode ::
            decode(decoder) ?, signature : :: bincode :: Decode ::
            decode(decoder) ?, calls : :: bincode :: Decode :: decode(decoder)
            ?, call_lines : :: bincode :: Decode :: decode(decoder) ?, types :
            :: bincode :: Decode :: decode(decoder) ?, imports : :: bincode ::
            Decode :: decode(decoder) ?, string_args : :: bincode :: Decode ::
            decode(decoder) ?, param_flows : :: bincode :: Decode ::
            decode(decoder) ?, param_types : :: bincode :: Decode ::
            decode(decoder) ?, field_types : :: bincode :: Decode ::
            decode(decoder) ?, local_types : :: bincode :: Decode ::
            decode(decoder) ?, let_call_bindings : :: bincode :: Decode ::
            decode(decoder) ?, return_type : :: bincode :: Decode ::
            decode(decoder) ?, field_accesses : :: bincode :: Decode ::
            decode(decoder) ?, enum_variants : :: bincode :: Decode ::
            decode(decoder) ?, is_test : :: bincode :: Decode ::
            decode(decoder) ?,
        })
    }
} impl < '__de, __Context > :: bincode :: BorrowDecode < '__de, __Context >
for ParsedChunk
{
    fn borrow_decode < __D : :: bincode :: de :: BorrowDecoder < '__de,
    Context = __Context > > (decoder : & mut __D) ->core :: result :: Result <
    Self, :: bincode :: error :: DecodeError >
    {
        core :: result :: Result ::
        Ok(Self
        {
            kind : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, name : :: bincode :: BorrowDecode ::<
            '_, __Context >:: borrow_decode(decoder) ?, file : :: bincode ::
            BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?, lines
            : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, signature : :: bincode :: BorrowDecode
            ::< '_, __Context >:: borrow_decode(decoder) ?, calls : :: bincode
            :: BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?,
            call_lines : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, types : :: bincode :: BorrowDecode ::<
            '_, __Context >:: borrow_decode(decoder) ?, imports : :: bincode
            :: BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?,
            string_args : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, param_flows : :: bincode :: BorrowDecode
            ::< '_, __Context >:: borrow_decode(decoder) ?, param_types : ::
            bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, field_types : :: bincode :: BorrowDecode
            ::< '_, __Context >:: borrow_decode(decoder) ?, local_types : ::
            bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, let_call_bindings : :: bincode ::
            BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?,
            return_type : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, field_accesses : :: bincode ::
            BorrowDecode ::< '_, __Context >:: borrow_decode(decoder) ?,
            enum_variants : :: bincode :: BorrowDecode ::< '_, __Context >::
            borrow_decode(decoder) ?, is_test : :: bincode :: BorrowDecode ::<
            '_, __Context >:: borrow_decode(decoder) ?,
        })
    }
}