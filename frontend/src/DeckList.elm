module DeckList exposing (FromServerMsg, Model, Msg(..), OtherMsg, fromServerDecoder, init, update, view)

import Html as H exposing (Html)
import Html.Attributes as A
import Html.Events as E
import Json.Decode as JD
import Json.Encode as JE


type alias Deck =
    { id : String
    , title : String
    , url : String
    , json : Maybe JD.Value
    }


type alias DeckModel =
    { deck : Deck
    , status : String
    }


type alias Model =
    { decks : List DeckModel }


type Msg
    = FromServer FromServerMsg
    | Other OtherMsg


type OtherMsg
    = DeleteDeck String
    | RebuildDeck String


type FromServerMsg
    = DeckList (List Deck)
    | Notification ( String, NotificationMsg )


type NotificationMsg
    = Loading
    | NewDeck Deck
    | RenderedCards { complete : Int, total : Int }
    | RenderedPages { complete : Int, total : Int }
    | Complete
    | Error (Maybe String)


init : ( Model, List String )
init =
    ( { decks = [] }
    , [ JE.object [ ( "tag", JE.string "get-decks" ) ]
            |> JE.encode 0
      ]
    )


view : Model -> Html OtherMsg
view model =
    H.div [ A.id "generated-decks" ]
        [ H.h3 [] [ H.text "Your decks:" ]
        , let
            sortedDecks =
                model.decks
                    |> List.sortBy (\d -> String.toLower d.deck.title)
          in
          case sortedDecks of
            [] ->
                H.p [] [ H.text "No decks loaded yet." ]

            _ ->
                H.ul [] (List.map viewDeck sortedDecks)
        ]


viewDeck : DeckModel -> Html OtherMsg
viewDeck { deck, status } =
    H.li []
        [ H.span [ A.class "deck-name" ]
            [ case status of
                "" ->
                    H.a [ A.href ("decks/" ++ deck.id ++ ".json"), A.target "_blank" ]
                        [ H.text deck.title ]

                _ ->
                    H.text deck.title
            , H.text " "
            , H.a [ A.href deck.url, A.target "_blank" ] [ H.text "🗃" ]
            ]
        , H.span [ A.class "deck-status" ]
            [ case status of
                "" ->
                    H.text ""

                _ ->
                    H.text status
            ]
        , H.button
            [ A.style "flex" "0 0 auto"
            , E.onClick (RebuildDeck deck.id)
            ]
            [ H.text "Rebuild" ]
        , H.button
            [ A.style "color" "red"
            , A.style "flex" "0 0 auto"
            , E.onClick (DeleteDeck deck.id)
            ]
            [ H.text "✕" ]
        ]


update : Msg -> Model -> ( Model, List String )
update msg model =
    case msg of
        FromServer (DeckList newDecks) ->
            ( { model | decks = List.map (\d -> { deck = d, status = "" }) newDecks }, [] )

        FromServer (Notification ( _, NewDeck d )) ->
            let
                decks =
                    { deck = d, status = "Loading…" }
                        :: List.filter (\dm -> dm.deck.id /= d.id) model.decks
                        |> List.sortBy (\dm -> ( dm.deck.title, dm.deck.url ))
            in
            ( { model | decks = decks }, [] )

        FromServer (Notification ( deckID, srvMsg )) ->
            let
                statusMsg =
                    case srvMsg of
                        Loading ->
                            "Loading…"

                        NewDeck _ ->
                            ""

                        RenderedCards { complete, total } ->
                            "Rendered " ++ String.fromInt complete ++ " of " ++ String.fromInt total ++ " cards"

                        RenderedPages { complete, total } ->
                            "Saved " ++ String.fromInt complete ++ " of " ++ String.fromInt total ++ " pages"

                        Complete ->
                            ""

                        Error (Just errMsg) ->
                            "Error from server while rendering:  " ++ errMsg

                        Error Nothing ->
                            "Unknown error from server while rendering"

                updateStatus dm =
                    if dm.deck.id == deckID || dm.deck.url == deckID then
                        { dm | status = statusMsg }

                    else
                        dm
            in
            ( { model | decks = List.map updateStatus model.decks }
            , []
            )

        Other (DeleteDeck id) ->
            let
                deleteMsg =
                    JE.object
                        [ ( "tag", JE.string "delete-deck" )
                        , ( "id", JE.string id )
                        ]
                        |> JE.encode 0

                remainingDecks =
                    List.filter (\dm -> dm.deck.id /= id) model.decks
            in
            ( { model | decks = remainingDecks }
            , [ deleteMsg ]
            )

        Other (RebuildDeck id) ->
            let
                rebuildMsg deck =
                    JE.object
                        [ ( "tag", JE.string "render-deck" )
                        , ( "url", JE.string deck.url )
                        ]
                        |> JE.encode 0

                maybeDeck =
                    model.decks
                        |> List.filter (\dm -> dm.deck.id == id)
                        |> List.head
                        |> Maybe.map .deck

                wsMsgs =
                    case maybeDeck of
                        Just deck ->
                            [ rebuildMsg deck ]

                        Nothing ->
                            []
            in
            ( model, wsMsgs )


deckDecoder : JD.Decoder Deck
deckDecoder =
    JD.map4 (\id title url json -> { id = id, title = title, url = url, json = json })
        (JD.field "id" JD.string)
        (JD.field "title" JD.string)
        (JD.field "url" JD.string)
        (JD.maybe (JD.field "json" JD.value))


notificationMsgDecoder : JD.Decoder ( String, NotificationMsg )
notificationMsgDecoder =
    let
        contentDecoder tag =
            case tag of
                "loading" ->
                    JD.map (\url -> ( url, Loading ))
                        (JD.field "url" JD.string)

                "new-deck" ->
                    JD.map (\d -> ( d.id, NewDeck d )) deckDecoder

                "rendering-images" ->
                    JD.map3 (\id complete total -> ( id, RenderedCards { complete = complete, total = total } ))
                        (JD.field "deck_id" JD.string)
                        (JD.field "rendered_cards" JD.int)
                        (JD.field "total_cards" JD.int)

                "saving-pages" ->
                    JD.map3 (\id complete total -> ( id, RenderedPages { complete = complete, total = total } ))
                        (JD.field "deck_id" JD.string)
                        (JD.field "saved_pages" JD.int)
                        (JD.field "total_pages" JD.int)

                "rendered" ->
                    JD.map (\id -> ( id, Complete ))
                        (JD.field "deck_id" JD.string)

                _ ->
                    JD.fail ("Unexpected tag in deck rendering notification from server: " ++ tag)

        handleLabel label =
            case label of
                "deck_rendering" ->
                    JD.field "data"
                        (JD.field "tag" JD.string
                            |> JD.andThen contentDecoder
                        )

                _ ->
                    JD.fail ("Unexpected notification label from server: " ++ label)
    in
    JD.field "label" JD.string
        |> JD.andThen handleLabel


fromServerDecoder : JD.Decoder FromServerMsg
fromServerDecoder =
    let
        handleTag tag =
            case tag of
                "deck-list" ->
                    JD.map DeckList (JD.field "decks" (JD.list deckDecoder))

                "notification" ->
                    JD.map Notification (JD.field "notification" notificationMsgDecoder)

                _ ->
                    JD.fail ("Unexpected tag from server: " ++ tag)
    in
    JD.field "tag" JD.string
        |> JD.andThen handleTag
