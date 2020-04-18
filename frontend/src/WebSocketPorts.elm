port module WebSocketPorts exposing (wsFromElmPort, wsToElmPort)

import Json.Decode as JD
import Json.Encode as JE


port wsFromElmPort : JE.Value -> Cmd msg


port wsToElmPort : (JD.Value -> msg) -> Sub msg


port saveJsonPort : { filename : String, jsonValue : JE.Value } -> Cmd msg
