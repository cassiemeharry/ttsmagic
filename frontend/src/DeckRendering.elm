module DeckRendering exposing (Model, Msg, init, update, view)

import Html as H exposing (Html)
import Html.Attributes as A
import Html.Events as E
import Json.Decode as JD
import Json.Encode as JE


type alias Model =
    String


type Msg
    = RenderDeck
    | SetDeckURL String


init : Model
init =
    -- "https://deckbox.org/sets/622934"
    ""


view : Model -> Html Msg
view model =
    H.form [ A.id "create-deck-form", E.onSubmit RenderDeck ]
        [ H.label [ A.for "create-url" ] [ H.text "URL:" ]
        , H.input
            [ A.id "create-url"
            , A.autofocus True
            , A.type_ "text"
            , A.placeholder "https://deckbox.org/sets/XXXXXX"
            , A.value model
            , E.onInput SetDeckURL
            ]
            []
        , H.input [ A.type_ "submit", A.value "Convert!" ] []
        ]


update : Msg -> Model -> ( Model, List String )
update msg model =
    case msg of
        RenderDeck ->
            ( ""
            , [ JE.object
                    [ ( "tag", JE.string "render-deck" )
                    , ( "url", JE.string model )
                    ]
                    |> JE.encode 0
              ]
            )

        SetDeckURL newURL ->
            ( newURL, [] )
