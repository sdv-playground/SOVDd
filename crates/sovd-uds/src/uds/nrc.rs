//! UDS Negative Response Codes (NRC)

use std::fmt;

/// UDS Negative Response Codes (NRC)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NegativeResponseCode {
    // General NRCs
    GeneralReject = 0x10,
    ServiceNotSupported = 0x11,
    SubFunctionNotSupported = 0x12,
    IncorrectMessageLengthOrFormat = 0x13,
    ResponseTooLong = 0x14,

    // Condition NRCs
    BusyRepeatRequest = 0x21,
    ConditionsNotCorrect = 0x22,

    // Sequence NRCs
    RequestSequenceError = 0x24,
    NoResponseFromSubnet = 0x25,
    FailurePreventsExecution = 0x26,

    // Request NRCs
    RequestOutOfRange = 0x31,
    SecurityAccessDenied = 0x33,
    InvalidKey = 0x35,
    ExceededNumberOfAttempts = 0x36,
    RequiredTimeDelayNotExpired = 0x37,

    // Upload/Download NRCs
    UploadDownloadNotAccepted = 0x70,
    TransferDataSuspended = 0x71,
    GeneralProgrammingFailure = 0x72,
    WrongBlockSequenceCounter = 0x73,

    // Response Pending
    ResponsePending = 0x78,

    // Sub-function NRCs
    SubFunctionNotSupportedInActiveSession = 0x7E,
    ServiceNotSupportedInActiveSession = 0x7F,

    // Vehicle specific
    RpmTooHigh = 0x81,
    RpmTooLow = 0x82,
    EngineRunning = 0x83,
    EngineNotRunning = 0x84,
    EngineRunTimeTooLow = 0x85,
    TemperatureTooHigh = 0x86,
    TemperatureTooLow = 0x87,
    VehicleSpeedTooHigh = 0x88,
    VehicleSpeedTooLow = 0x89,
    ThrottleTooHigh = 0x8A,
    ThrottleTooLow = 0x8B,
    TransmissionNotInNeutral = 0x8C,
    TransmissionNotInGear = 0x8D,
    BrakeSwitchNotClosed = 0x8F,
    ShifterNotInPark = 0x90,
    TorqueConverterClutchLocked = 0x91,
    VoltageTooHigh = 0x92,
    VoltageTooLow = 0x93,

    /// Unknown/reserved NRC
    Unknown(u8),
}

impl From<u8> for NegativeResponseCode {
    fn from(value: u8) -> Self {
        match value {
            0x10 => Self::GeneralReject,
            0x11 => Self::ServiceNotSupported,
            0x12 => Self::SubFunctionNotSupported,
            0x13 => Self::IncorrectMessageLengthOrFormat,
            0x14 => Self::ResponseTooLong,
            0x21 => Self::BusyRepeatRequest,
            0x22 => Self::ConditionsNotCorrect,
            0x24 => Self::RequestSequenceError,
            0x25 => Self::NoResponseFromSubnet,
            0x26 => Self::FailurePreventsExecution,
            0x31 => Self::RequestOutOfRange,
            0x33 => Self::SecurityAccessDenied,
            0x35 => Self::InvalidKey,
            0x36 => Self::ExceededNumberOfAttempts,
            0x37 => Self::RequiredTimeDelayNotExpired,
            0x70 => Self::UploadDownloadNotAccepted,
            0x71 => Self::TransferDataSuspended,
            0x72 => Self::GeneralProgrammingFailure,
            0x73 => Self::WrongBlockSequenceCounter,
            0x78 => Self::ResponsePending,
            0x7E => Self::SubFunctionNotSupportedInActiveSession,
            0x7F => Self::ServiceNotSupportedInActiveSession,
            0x81 => Self::RpmTooHigh,
            0x82 => Self::RpmTooLow,
            0x83 => Self::EngineRunning,
            0x84 => Self::EngineNotRunning,
            0x85 => Self::EngineRunTimeTooLow,
            0x86 => Self::TemperatureTooHigh,
            0x87 => Self::TemperatureTooLow,
            0x88 => Self::VehicleSpeedTooHigh,
            0x89 => Self::VehicleSpeedTooLow,
            0x8A => Self::ThrottleTooHigh,
            0x8B => Self::ThrottleTooLow,
            0x8C => Self::TransmissionNotInNeutral,
            0x8D => Self::TransmissionNotInGear,
            0x8F => Self::BrakeSwitchNotClosed,
            0x90 => Self::ShifterNotInPark,
            0x91 => Self::TorqueConverterClutchLocked,
            0x92 => Self::VoltageTooHigh,
            0x93 => Self::VoltageTooLow,
            other => Self::Unknown(other),
        }
    }
}

impl From<NegativeResponseCode> for u8 {
    fn from(nrc: NegativeResponseCode) -> Self {
        match nrc {
            NegativeResponseCode::GeneralReject => 0x10,
            NegativeResponseCode::ServiceNotSupported => 0x11,
            NegativeResponseCode::SubFunctionNotSupported => 0x12,
            NegativeResponseCode::IncorrectMessageLengthOrFormat => 0x13,
            NegativeResponseCode::ResponseTooLong => 0x14,
            NegativeResponseCode::BusyRepeatRequest => 0x21,
            NegativeResponseCode::ConditionsNotCorrect => 0x22,
            NegativeResponseCode::RequestSequenceError => 0x24,
            NegativeResponseCode::NoResponseFromSubnet => 0x25,
            NegativeResponseCode::FailurePreventsExecution => 0x26,
            NegativeResponseCode::RequestOutOfRange => 0x31,
            NegativeResponseCode::SecurityAccessDenied => 0x33,
            NegativeResponseCode::InvalidKey => 0x35,
            NegativeResponseCode::ExceededNumberOfAttempts => 0x36,
            NegativeResponseCode::RequiredTimeDelayNotExpired => 0x37,
            NegativeResponseCode::UploadDownloadNotAccepted => 0x70,
            NegativeResponseCode::TransferDataSuspended => 0x71,
            NegativeResponseCode::GeneralProgrammingFailure => 0x72,
            NegativeResponseCode::WrongBlockSequenceCounter => 0x73,
            NegativeResponseCode::ResponsePending => 0x78,
            NegativeResponseCode::SubFunctionNotSupportedInActiveSession => 0x7E,
            NegativeResponseCode::ServiceNotSupportedInActiveSession => 0x7F,
            NegativeResponseCode::RpmTooHigh => 0x81,
            NegativeResponseCode::RpmTooLow => 0x82,
            NegativeResponseCode::EngineRunning => 0x83,
            NegativeResponseCode::EngineNotRunning => 0x84,
            NegativeResponseCode::EngineRunTimeTooLow => 0x85,
            NegativeResponseCode::TemperatureTooHigh => 0x86,
            NegativeResponseCode::TemperatureTooLow => 0x87,
            NegativeResponseCode::VehicleSpeedTooHigh => 0x88,
            NegativeResponseCode::VehicleSpeedTooLow => 0x89,
            NegativeResponseCode::ThrottleTooHigh => 0x8A,
            NegativeResponseCode::ThrottleTooLow => 0x8B,
            NegativeResponseCode::TransmissionNotInNeutral => 0x8C,
            NegativeResponseCode::TransmissionNotInGear => 0x8D,
            NegativeResponseCode::BrakeSwitchNotClosed => 0x8F,
            NegativeResponseCode::ShifterNotInPark => 0x90,
            NegativeResponseCode::TorqueConverterClutchLocked => 0x91,
            NegativeResponseCode::VoltageTooHigh => 0x92,
            NegativeResponseCode::VoltageTooLow => 0x93,
            NegativeResponseCode::Unknown(v) => v,
        }
    }
}

impl fmt::UpperHex for NegativeResponseCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value: u8 = (*self).into();
        fmt::UpperHex::fmt(&value, f)
    }
}

impl fmt::Display for NegativeResponseCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GeneralReject => write!(f, "GeneralReject"),
            Self::ServiceNotSupported => write!(f, "ServiceNotSupported"),
            Self::SubFunctionNotSupported => write!(f, "SubFunctionNotSupported"),
            Self::IncorrectMessageLengthOrFormat => write!(f, "IncorrectMessageLengthOrFormat"),
            Self::ResponseTooLong => write!(f, "ResponseTooLong"),
            Self::BusyRepeatRequest => write!(f, "BusyRepeatRequest"),
            Self::ConditionsNotCorrect => write!(f, "ConditionsNotCorrect"),
            Self::RequestSequenceError => write!(f, "RequestSequenceError"),
            Self::NoResponseFromSubnet => write!(f, "NoResponseFromSubnet"),
            Self::FailurePreventsExecution => write!(f, "FailurePreventsExecution"),
            Self::RequestOutOfRange => write!(f, "RequestOutOfRange"),
            Self::SecurityAccessDenied => write!(f, "SecurityAccessDenied"),
            Self::InvalidKey => write!(f, "InvalidKey"),
            Self::ExceededNumberOfAttempts => write!(f, "ExceededNumberOfAttempts"),
            Self::RequiredTimeDelayNotExpired => write!(f, "RequiredTimeDelayNotExpired"),
            Self::UploadDownloadNotAccepted => write!(f, "UploadDownloadNotAccepted"),
            Self::TransferDataSuspended => write!(f, "TransferDataSuspended"),
            Self::GeneralProgrammingFailure => write!(f, "GeneralProgrammingFailure"),
            Self::WrongBlockSequenceCounter => write!(f, "WrongBlockSequenceCounter"),
            Self::ResponsePending => write!(f, "ResponsePending"),
            Self::SubFunctionNotSupportedInActiveSession => {
                write!(f, "SubFunctionNotSupportedInActiveSession")
            }
            Self::ServiceNotSupportedInActiveSession => {
                write!(f, "ServiceNotSupportedInActiveSession")
            }
            Self::RpmTooHigh => write!(f, "RpmTooHigh"),
            Self::RpmTooLow => write!(f, "RpmTooLow"),
            Self::EngineRunning => write!(f, "EngineRunning"),
            Self::EngineNotRunning => write!(f, "EngineNotRunning"),
            Self::EngineRunTimeTooLow => write!(f, "EngineRunTimeTooLow"),
            Self::TemperatureTooHigh => write!(f, "TemperatureTooHigh"),
            Self::TemperatureTooLow => write!(f, "TemperatureTooLow"),
            Self::VehicleSpeedTooHigh => write!(f, "VehicleSpeedTooHigh"),
            Self::VehicleSpeedTooLow => write!(f, "VehicleSpeedTooLow"),
            Self::ThrottleTooHigh => write!(f, "ThrottleTooHigh"),
            Self::ThrottleTooLow => write!(f, "ThrottleTooLow"),
            Self::TransmissionNotInNeutral => write!(f, "TransmissionNotInNeutral"),
            Self::TransmissionNotInGear => write!(f, "TransmissionNotInGear"),
            Self::BrakeSwitchNotClosed => write!(f, "BrakeSwitchNotClosed"),
            Self::ShifterNotInPark => write!(f, "ShifterNotInPark"),
            Self::TorqueConverterClutchLocked => write!(f, "TorqueConverterClutchLocked"),
            Self::VoltageTooHigh => write!(f, "VoltageTooHigh"),
            Self::VoltageTooLow => write!(f, "VoltageTooLow"),
            Self::Unknown(v) => write!(f, "Unknown(0x{:02X})", v),
        }
    }
}
