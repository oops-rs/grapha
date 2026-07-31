# frozen_string_literal: true

require_relative "helpers"

# Coordinates the Ruby fixture.
class RubyWorker < BaseWorker
  attr_reader :label

  def initialize
    @on_ready = -> { report_ready }
    @label = format_label("ruby")
  end

  def run
    @on_ready.call
  end

  private

  def report_ready
  end
end

worker = RubyWorker.new
worker.run
