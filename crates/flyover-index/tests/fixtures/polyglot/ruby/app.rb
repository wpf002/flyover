require "json"
require_relative "helper"

module Demo
  class Widget
    def render
      "w"
    end
  end

  def self.build
    Widget.new
  end
end
