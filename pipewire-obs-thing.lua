
local args = ({...})[1]:parse()

-- local function dump(o)
--    if type(o) == 'table' then
--       local s = '{ '
--       for k,v in pairs(o) do
--          if type(k) ~= 'number' then k = '"'..k..'"' end
--          s = s .. '['..k..'] = ' .. dump(v) .. ',\n'
--       end
--       return s .. '} '
--    else
--       return tostring(o)
--    end
-- end

-- from https://bennett.dev/auto-link-pipewire-ports-wireplumber/
local function link_ports(output_port, input_port)
	local link = Link("link-factory", {
		-- The node and port to connect from
		["link.output.node"] = output_port.properties["node.id"],
		["link.output.port"] = output_port.properties["object.id"],

		-- The node and port to connect to
		["link.input.node"] = input_port.properties["node.id"],
		["link.input.port"] = input_port.properties["object.id"],

		-- I found that not having this entry in the args would fail
		-- to create the link. Setting it to nil seems to work
		["object.id"] = nil,

		-- I'm not completely sure what this does but it seems to
		-- make the link much more reliable
		["object.linger"] = true
	})
	link:activate(1)
end

local links = ObjectManager {
  Interest { type = "link" },
}

local obs = ObjectManager {
  Interest {
    type = "node",
    Constraint { "node.name", "=", "OBS Studio: audio" },
  },
}

-- this is the only fucking ObjectManager config that does actually receive object-added
-- events for noita.exe - a single benefit being that we actually filter by x11 display lol
--
-- if you have another ObjectManager listen to object-added, or more interests here,
-- it DOESNT FUCKING WORK ANYMORE, spooky action at a distance my ass
local offloaded = ObjectManager {
  Interest {
    type = "node",
    Constraint { "window.x11.display", "=", args.display, type = "pw" },
  },
}

local function reconnect_ports(node)
  local obs_node = obs:lookup()
  if not obs_node then
    print('no OBS!')
    return
  end

  node = node or offloaded:lookup()

  local fl_port = node:lookup_port {
    Constraint { "port.name", "=", "output_FL" },
  }
  local fr_port = node:lookup_port {
    Constraint { "port.name", "=", "output_FR" },
  }

  if not fl_port or not fr_port then
    print('fml, no ports again')
    Core.timeout_add(100, function()
      reconnect_ports()
    end)
    return
  end

  local fl_id = fl_port.properties["object.id"]
  local fr_id = fr_port.properties["object.id"]

  for link in links:iterate { Constraint { "link.output.port", "c", fl_id, fr_id, type = 'pw' } } do
    print('destroying link')
    link:request_destroy()
  end

  local in1 = obs_node:lookup_port {
    Constraint { "port.name", "=", "in_1" }
  }
  local in2 = obs_node:lookup_port {
    Constraint { "port.name", "=", "in_2" }
  }
  link_ports(fl_port, in1)
  link_ports(fr_port, in2)
end

offloaded:connect('object-added', function(_, node)
  print('node added on '..args.display..' - '..(node.properties['node.name'] or 'nil')..', id: '..(node.properties['object.id'] or 'nil'))

  -- You think this is brittle dogshit? you're absolutely correct!!
  -- good luck making that trash behave in an adequate manner properly
  -- What fucking point in overengineering this whole interest system bs
  -- if it doesn't fucking work if you look at it wrong??..
  Core.timeout_add(100, function()
    reconnect_ports(node)
  end)
end)

links:activate()
obs:activate()
offloaded:activate()
