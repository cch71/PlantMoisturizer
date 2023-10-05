#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::cell::RefCell;
use critical_section::Mutex;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X18_BOLD},
        MonoTextStyleBuilder,
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use esp_backtrace as _;
use esp_println::println;
use hal::{
    adc::{AdcConfig, Attenuation, ADC, ADC1},
    clock::ClockControl,
    embassy::{self, executor::Executor},
    gpio::{Analog, Gpio21, Gpio4, Input, Output, PushPull},
    i2c::I2C,
    ledc::{
        channel::{self, ChannelIFace},
        timer::{self, TimerIFace},
        LSGlobalClkSource, LowSpeed, LEDC,
    },
    peripherals::{Peripherals, I2C0},
    prelude::*,
    IO,
};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};
use static_cell::make_static;

static MSG_Q: Channel<CriticalSectionRawMutex, (u16, u32), 5> = Channel::new();

///////////////////////////////////////////////////////////////////////////
///
#[embassy_executor::task]
async fn run_display(i2c: I2C<'static, I2C0>) {
    // Initialize display
    let interface = I2CDisplayInterface::new(i2c);

    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().unwrap();

    // Specify different text styles
    let text_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let text_style_big = MonoTextStyleBuilder::new()
        .font(&FONT_9X18_BOLD)
        .text_color(BinaryColor::On)
        .build();

    // Fill display bufffer with a centered text with two lines (and two text
    // styles)
    Text::with_alignment(
        "Plant Minder",
        display.bounding_box().center().x_axis() + Point::new(0, 10),
        text_style_big,
        Alignment::Center,
    )
    .draw(&mut display)
    .unwrap();

    // Write buffer to display
    display.flush().unwrap();

    let center_pt = display.bounding_box().center();

    let outline_style = PrimitiveStyleBuilder::new()
        .stroke_color(BinaryColor::On)
        .stroke_width(1)
        .fill_color(BinaryColor::Off)
        .build();

    let filled_style = PrimitiveStyleBuilder::new()
        .stroke_color(BinaryColor::On)
        .stroke_width(1)
        .fill_color(BinaryColor::On)
        .build();

    loop {
        display
            .fill_solid(
                &Rectangle::new(Point::new(0, 16), Size::new(128, 48)),
                BinaryColor::Off,
            )
            .unwrap();

        let (val, pct) = MSG_Q.receive().await;

        use core::fmt::Write;
        use heapless::String;
        let mut data = String::<64>::new();
        let _ = write!(data, "Raw Val {:04}", val);

        Text::with_alignment(data.as_str(), center_pt, text_style, Alignment::Center)
            .draw(&mut display)
            .unwrap();

        // Outline
        Rectangle::new(Point::new(14, 40), Size::new(100, 20))
            .into_styled(outline_style)
            .draw(&mut display)
            .unwrap();

        // Filled in percent
        Rectangle::new(Point::new(14, 40), Size::new(pct, 20))
            .into_styled(filled_style)
            .draw(&mut display)
            .unwrap();

        // Draw line for what over watered would be. 29% is over-watered
        let over_watered_line_x = 14 + 29;
        Line::new(
            Point::new(over_watered_line_x, 38),
            Point::new(over_watered_line_x, 62),
        )
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(&mut display)
        .unwrap();

        display.flush().unwrap();
    }

    // Clear display buffer
    // display.clear(BinaryColor::Off).unwrap();
}

///////////////////////////////////////////////////////////////////////////
///
#[embassy_executor::task]
async fn blinker(mut led: Gpio21<Output<PushPull>>) {
    // critical_section::with(|cs| {
    //     let ledc = LEDC_MTX.borrow_ref_mut(cs).as_mut().unwrap();
    //     ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    //     let mut lstimer0 = ledc.get_timer::<LowSpeed>(timer::Number::Timer0);
    //     lstimer0
    //         .configure(timer::config::Config {
    //             duty: timer::config::Duty::Duty5Bit,
    //             clock_source: timer::LSClockSource::APBClk,
    //             frequency: 24u32.kHz(),
    //         })
    //         .unwrap();

    //     let mut channel0 = ledc.get_channel(channel::Number::Channel0, led);
    //     channel0
    //         .configure(channel::config::Config {
    //             timer: &lstimer0,
    //             duty_pct: 10,
    //             pin_config: channel::config::PinConfig::PushPull,
    //         })
    //         .unwrap();

    //     channel0.start_duty_fade(0, 100, 2000).expect_err(
    //         "Fading from 0% to 100%, at 24kHz and 5-bit resolution, over 2 seconds, should fail",
    //     );

    //     loop {
    //         // Set up a breathing LED: fade from off to on over a second, then
    //         // from on back off over the next second.  Then loop.
    //         channel0.start_duty_fade(0, 100, 1000).unwrap();
    //         while channel0.is_duty_fade_running() {}
    //         channel0.start_duty_fade(100, 0, 1000).unwrap();
    //         while channel0.is_duty_fade_running() {}
    //     }
    // });

    // Setup LED User Pin for Seeed XIAO
    led.set_high().unwrap();

    loop {
        led.toggle().unwrap();
        Timer::after(Duration::from_millis(500)).await;
    }
}

///////////////////////////////////////////////////////////////////////////
///
#[embassy_executor::task]
async fn soil_probe_reader(mut adc_pin: Gpio4<Analog>, mut analog: hal::analog::AvailableAnalog) {
    let atten = Attenuation::Attenuation11dB;

    let mut adc1_config = AdcConfig::new();

    type AdcCal = ();
    let mut adc_pin = adc1_config.enable_pin_with_cal::<_, AdcCal>(adc_pin, atten);
    let mut adc1 = ADC::<ADC1>::adc(analog.adc1, adc1_config).unwrap();

    loop {
        // 4095 (Dry) 3100 (Wet)
        let pin_value: u16 = nb::block!(adc1.read(&mut adc_pin)).unwrap();
        // let pin_value_mv = pin_value as u32 * atten.ref_mv() as u32 / 4096;
        // println!("ADC reading = {pin_value} ({pin_value_mv} mV)");

        let pct = 100 - ((pin_value as f32 / 4096.0) * 100.0) as u32;

        MSG_Q.send((pin_value, pct)).await;
        Timer::after(Duration::from_millis(1000)).await;
    }
}

///////////////////////////////////////////////////////////////////////////
///
#[entry]
fn main() -> ! {
    let peripherals = Peripherals::take();
    let mut system = peripherals.SYSTEM.split();
    let clocks = ClockControl::max(system.clock_control).freeze();

    // Initialize Embassy using systimer
    embassy::init(
        &clocks,
        hal::systimer::SystemTimer::new(peripherals.SYSTIMER),
    );

    let io = IO::new(peripherals.GPIO, peripherals.IO_MUX);

    // Create LED Pin
    let led = io.pins.gpio21.into_push_pull_output();

    // Create ADC instances
    let analog = peripherals.SENS.split();
    let adc_pin = io.pins.gpio4.into_analog();

    // let ledc = make_static!(LEDC::new(
    //     peripherals.LEDC,
    //     &clocks,
    //     &mut system.peripheral_clock_control,
    // ));
    // // CLocks are borrowed by LEDC right now so this may be only way to get it to work
    // critical_section::with(|cs| LEDC_MTX.borrow_ref_mut(cs).replace(ledc));

    // Initialize the I2C bus for communicating with the LCD Display
    let i2c = I2C::new(
        peripherals.I2C0,
        io.pins.gpio5, //SDA
        io.pins.gpio6, //SCL
        400u32.kHz(),
        &mut system.peripheral_clock_control,
        &clocks,
    );

    // setup logger
    // To change the log_level change the env section in .cargo/config.toml
    // or remove it and set ESP_LOGLEVEL manually before running cargo run
    // this requires a clean rebuild because of https://github.com/rust-lang/cargo/issues/10358
    esp_println::logger::init_logger_from_env();
    log::info!("Logger is setup");
    println!("Hello world!");

    let executor = make_static!(Executor::new());
    executor.run(|spawner| {
        spawner.spawn(blinker(led)).ok();
        spawner.spawn(run_display(i2c)).ok();
        spawner.spawn(soil_probe_reader(adc_pin, analog)).ok();
    });
}
