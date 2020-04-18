module Main exposing (Model, Msg(..), init, main, subscriptions, update, view)

import Browser
import DeckList as DL
import DeckRendering as DR
import Html as H exposing (Html)
import Html.Attributes as A
import Html.Events as E
import Json.Decode as JD
import Json.Encode as JE exposing (Value)
import WebSocket as WS
import WebSocketPorts exposing (wsFromElmPort, wsToElmPort)


main =
    Browser.document
        { init = init
        , view = view
        , update = update
        , subscriptions = subscriptions
        }


type alias Flags =
    { socketURL : String }


type alias Model =
    { deckList : DL.Model
    , rendering : DR.Model
    }


type Msg
    = DeckList DL.OtherMsg
    | DeckRendering DR.Msg
    | FromServer (Result FromServerError FromServerMsg)


type FromServerError
    = JsonDecodeError JD.Error
    | WSError String


type FromServerMsg
    = DeckListFromServer DL.FromServerMsg


init : Flags -> ( Model, Cmd Msg )
init flags =
    let
        ( deckList, dlInitMsgs ) =
            DL.init

        rendering =
            DR.init

        wsCmds =
            WS.Connect
                { name = "server"
                , address = flags.socketURL
                , protocol = "ttsmagic-ws-protocol-v1"
                }
                :: List.map (\c -> WS.Send { name = "server", content = c }) dlInitMsgs
                |> List.map wsSend
                |> Cmd.batch
    in
    ( { deckList = deckList
      , rendering = DR.init
      }
    , wsCmds
    )


view : Model -> Browser.Document Msg
view model =
    { title = "ttsmagic.cards"
    , body =
        [ H.div
            [ A.id "content"

            -- , A.style "max-width" "980px"
            -- , A.style "margin" "0 auto"
            -- , A.style "padding" "10px"
            -- , A.style "padding-bottom" "40px"
            -- , A.style "text-align" "center"
            ]
            [ H.h1 [] [ H.text "MtG → Tabletop Simulator Deck Builder" ]
            , DR.view model.rendering |> H.map DeckRendering
            , DL.view model.deckList |> H.map DeckList
            , H.footer []
                [ H.a
                    [ A.href "/beta/logout/" ]
                    [ H.text "Sign out" ]
                ]
            ]
        ]
    }


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    let
        translateOutboundWSSingle : String -> Cmd Msg
        translateOutboundWSSingle content =
            { name = "server", content = content }
                |> WS.Send
                |> wsSend

        translateOutboundWS =
            List.map translateOutboundWSSingle >> Cmd.batch
    in
    case msg of
        DeckList dlMsg ->
            let
                ( dlModel, outboundWS ) =
                    DL.update (DL.Other dlMsg) model.deckList
            in
            ( { model | deckList = dlModel }
            , translateOutboundWS outboundWS
            )

        DeckRendering drMsg ->
            let
                ( drModel, outboundWS ) =
                    DR.update drMsg model.rendering
            in
            ( { model | rendering = drModel }
            , translateOutboundWS outboundWS
            )

        FromServer (Ok (DeckListFromServer dlMsg)) ->
            let
                ( dlModel, outboundWSMsgs ) =
                    DL.update (DL.FromServer dlMsg) model.deckList
            in
            ( { model | deckList = dlModel }
            , translateOutboundWS outboundWSMsgs
            )

        FromServer (Err decodeErr) ->
            Debug.log ("Failed to decode WebSocket message: " ++ Debug.toString decodeErr)
                ( model, Cmd.none )


wsSend : WS.WebSocketCmd -> Cmd Msg
wsSend =
    WS.send wsFromElmPort


fromServerMessageDecoder : JD.Decoder FromServerMsg
fromServerMessageDecoder =
    JD.oneOf
        [ JD.map DeckListFromServer DL.fromServerDecoder
        ]


parseServerWSMsg : Result JD.Error WS.WebSocketMsg -> Result FromServerError FromServerMsg
parseServerWSMsg wsMsgResult =
    case wsMsgResult of
        Ok (WS.Error { error }) ->
            Err (WSError error)

        Ok (WS.Data { data }) ->
            JD.decodeString fromServerMessageDecoder data
                |> Result.mapError JsonDecodeError

        Err jdError ->
            Err (JsonDecodeError jdError)


subscriptions : Model -> Sub Msg
subscriptions model =
    wsToElmPort <| WS.receive (parseServerWSMsg >> FromServer)
